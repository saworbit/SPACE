use anyhow::{anyhow, bail, Context, Result};
use common::scrub::{Damage, DamageKind, ScrubKind, ScrubTimes};
use common::{Capsule, CapsuleId, ContentHash, SegmentId};
use redb::{
    Database, DatabaseError, Durability, ReadableDatabase, ReadableTable, ReadableTableMetadata,
    TableDefinition, WriteTransaction,
};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::journal::JournalPosition;
use common::format::JournalRecord;

const NEXT_SEGMENT_KEY: &[u8] = b"next_segment_id";
/// Key in the counters table for the journal position the index has reached.
const JOURNAL_POSITION_KEY: &[u8] = b"journal_position";

/// What a name points at: a capsule, and the metadata kept with the name
/// (`docs/on-disk-format.md` §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub capsule_id: CapsuleId,
    pub metadata: Vec<(String, String)>,
}

/// The registry's index of the capsule journal. The storage engine under it
/// can change without the registry noticing (#244).
pub trait MetadataStore: Send + Sync {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>>;
    fn list_capsules(&self, limit: usize, start_after: Option<CapsuleId>) -> Result<Vec<Capsule>>;
    #[allow(dead_code)]
    fn add_deduped_bytes(&self, id: &CapsuleId, bytes: u64) -> Result<()>;

    fn allocate_segment_id(&self) -> Result<SegmentId>;
    /// Raise the segment counter to at least `floor`.
    fn ensure_segment_floor(&self, floor: u64) -> Result<()>;

    /// Apply one journal record, or a whole batch, and record `position`, in
    /// one transaction.
    fn apply_journal(&self, record: &JournalRecord, position: &JournalPosition) -> Result<()>;
    /// Record `position` without applying anything.
    fn set_journal_position(&self, position: &JournalPosition) -> Result<()>;
    fn journal_position(&self) -> Result<Option<JournalPosition>>;
    /// Empty everything the journal rebuilds: capsules, the content index,
    /// ref counts, names, and the journal position.
    fn clear_index(&self) -> Result<()>;
    /// Whether the index holds capsules, or S3 names from before the journal
    /// held names.
    fn has_records(&self) -> Result<bool>;
    /// How many capsule entries reference each segment.
    fn segment_refs(&self) -> Result<std::collections::HashMap<SegmentId, u32>>;
    /// When each segment was last scrubbed (#134). It isn't journal state,
    /// and an index rebuild keeps it: segment ids are never reused.
    fn scrub_times(&self) -> Result<Vec<(SegmentId, ScrubTimes)>>;
    /// Set segments' scrub times, or forget a segment's with `None`.
    fn put_scrub_times(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> Result<()>;
    /// Remove `hash` from the content index if it points at `segment` and no
    /// capsule references `segment`.
    fn deregister_unreferenced(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool>;

    fn get_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>>;
    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>>;

    /// Names (`docs/on-disk-format.md` §5.5–§5.6). Each namespace maps to its
    /// creation time, and each name to the capsule it points at and its
    /// metadata. Only applying journal records changes them.
    fn get_namespace(&self, ns: &str) -> Result<Option<u64>>;
    /// Namespaces that start with `prefix` and sort after `start_after`,
    /// in byte order, with their creation times.
    fn list_namespaces(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>>;
    fn get_name(&self, ns: &str, name: &str) -> Result<Option<Binding>>;
    /// Every name that points at `capsule`, as (namespace, name) (#138).
    fn names_of(&self, capsule: CapsuleId) -> Result<Vec<(String, String)>>;
    /// Names in `ns` that start with `prefix` and sort after `start_after`,
    /// in byte order.
    fn list_names(
        &self,
        ns: &str,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Binding)>>;
}

/// The index in one redb database file, `space.db` (#244). Every table maps
/// byte strings to byte strings, in byte order; the encodings are below.
pub struct RedbStore {
    db: Database,
}

type Def = TableDefinition<'static, &'static [u8], &'static [u8]>;
type Tbl<'txn> = redb::Table<'txn, &'static [u8], &'static [u8]>;

/// Capsule id to its capsule record.
const CAPSULES: Def = TableDefinition::new("capsules");
/// Content hash to the segment dedup reuses for it: the lowest numbered of
/// its `candidates` (#188).
const CONTENT: Def = TableDefinition::new("content");
/// Segment id to the number of capsule entries that reference it.
const REFS: Def = TableDefinition::new("refs");
/// Segment id to its content hash and the number of entries in live dedupe
/// capsules that reference it, for segments with any.
const INDEXED: Def = TableDefinition::new("indexed");
/// Content hash to every segment that holds it and that a live dedupe
/// capsule references, lowest first.
const CANDIDATES: Def = TableDefinition::new("candidates");
/// The live capsules written with dedupe on.
const DEDUPED: Def = TableDefinition::new("deduped");
/// The segment counter and the journal position.
const COUNTERS: Def = TableDefinition::new("counters");
/// Namespace to its creation time.
const NAMESPACES: Def = TableDefinition::new("namespaces");
/// Namespace and name ([`name_key`]) to the capsule the name points at.
const NAMES: Def = TableDefinition::new("names");
/// Each name under the capsule it points at ([`capsule_name_key`]), so
/// deleting a capsule can unbind its names (#138).
const CAPSULE_NAMES: Def = TableDefinition::new("capsule_names");
/// Segment id to when it was last light- and deep-scrubbed. Not journal
/// state, so a rebuild keeps it.
const SCRUB_SCHEDULE: Def = TableDefinition::new("scrub_schedule");

/// The tables a rebuild empties and the journal fills again.
const INDEX_TABLES: [Def; 9] = [
    CAPSULES,
    CONTENT,
    REFS,
    INDEXED,
    CANDIDATES,
    DEDUPED,
    NAMESPACES,
    NAMES,
    CAPSULE_NAMES,
];

/// Index key for a name. Namespaces can't contain NUL, so the separator
/// keeps every namespace's names apart.
fn name_key(ns: &str, name: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(ns.len() + 1 + name.len());
    out.extend_from_slice(ns.as_bytes());
    out.push(0);
    out.extend_from_slice(name.as_bytes());
    out
}

/// Index key for a name under the capsule it points at: the capsule id's 16
/// bytes, then the name's [`name_key`].
fn capsule_name_key(capsule: CapsuleId, name_key: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(16 + name_key.len());
    out.extend_from_slice(capsule.as_uuid().as_bytes());
    out.extend_from_slice(name_key);
    out
}

// The index's record encodings (#152). Numbers are little-endian. A capsule
// record, and a name's metadata, are JSON, as the journal keeps a policy.
// bincode, which encoded them before, is unmaintained (RUSTSEC-2025-0141).
// Segment ids, reference counts, the segment counter and scrub times read
// the same either way, so the scrub schedule survives the change. The rest
// of the index is rebuilt from the journal (`journal::INDEX_VERSION` 6).

fn segment_key(id: SegmentId) -> [u8; 8] {
    id.0.to_le_bytes()
}

fn capsule_key(id: CapsuleId) -> [u8; 16] {
    *id.as_uuid().as_bytes()
}

/// A content hash's key: its text, which is hex.
fn hash_key(hash: &ContentHash) -> Vec<u8> {
    hash.0.as_bytes().to_vec()
}

fn decode_hash(bytes: &[u8]) -> Result<ContentHash> {
    Ok(ContentHash(
        String::from_utf8(bytes.to_vec()).context("a content hash isn't text")?,
    ))
}

fn fixed<const N: usize>(bytes: &[u8], what: &str) -> Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| anyhow!("{what} is {} bytes, not {N}", bytes.len()))
}

fn decode_u64(bytes: &[u8], what: &str) -> Result<u64> {
    Ok(u64::from_le_bytes(fixed(bytes, what)?))
}

fn decode_u32(bytes: &[u8], what: &str) -> Result<u32> {
    Ok(u32::from_le_bytes(fixed(bytes, what)?))
}

fn decode_segment(bytes: &[u8]) -> Result<SegmentId> {
    decode_u64(bytes, "a segment id").map(SegmentId)
}

fn encode_capsule(capsule: &Capsule) -> Result<Vec<u8>> {
    serde_json::to_vec(capsule).context("can't encode a capsule record")
}

fn decode_capsule(bytes: &[u8]) -> Result<Capsule> {
    serde_json::from_slice(bytes).context("can't decode a capsule record")
}

/// A name's index record: its capsule's 16 bytes, then its metadata as
/// JSON.
fn encode_binding(capsule_id: CapsuleId, metadata: &[(String, String)]) -> Result<Vec<u8>> {
    let mut out = capsule_key(capsule_id).to_vec();
    serde_json::to_writer(&mut out, metadata).context("can't encode a name's metadata")?;
    Ok(out)
}

fn decode_binding(bytes: &[u8]) -> Result<Binding> {
    if bytes.len() < 16 {
        bail!("a name record is {} bytes, under 16", bytes.len());
    }
    let (id, metadata) = bytes.split_at(16);
    Ok(Binding {
        capsule_id: CapsuleId::from_uuid(uuid::Uuid::from_bytes(fixed(id, "a capsule id")?)),
        metadata: serde_json::from_slice(metadata).context("can't decode a name's metadata")?,
    })
}

/// A segment's `indexed` record: its dedupe references, then its hash.
fn encode_dedupe_refs(hash: &ContentHash, count: u32) -> Vec<u8> {
    let mut out = count.to_le_bytes().to_vec();
    out.extend_from_slice(hash.0.as_bytes());
    out
}

fn decode_dedupe_refs(bytes: &[u8]) -> Result<(ContentHash, u32)> {
    if bytes.len() < 4 {
        bail!(
            "a dedupe reference record is {} bytes, under 4",
            bytes.len()
        );
    }
    let (count, hash) = bytes.split_at(4);
    Ok((
        decode_hash(hash)?,
        decode_u32(count, "a dedupe reference count")?,
    ))
}

/// A hash's `candidates` record: each segment's id.
fn encode_segments(segments: &[SegmentId]) -> Vec<u8> {
    segments.iter().flat_map(|s| segment_key(*s)).collect()
}

fn decode_segments(bytes: &[u8]) -> Result<Vec<SegmentId>> {
    if !bytes.len().is_multiple_of(8) {
        bail!(
            "a candidates record is {} bytes, not a multiple of 8",
            bytes.len()
        );
    }
    Ok(bytes
        .as_chunks::<8>()
        .0
        .iter()
        .map(|id| SegmentId(u64::from_le_bytes(*id)))
        .collect())
}

/// The journal position, 36 bytes: the index version, the store id, the
/// generation and the offset.
fn encode_position(position: &JournalPosition) -> Vec<u8> {
    let mut out = Vec::with_capacity(36);
    out.extend_from_slice(&position.index_version.to_le_bytes());
    out.extend_from_slice(position.store_id.as_bytes());
    out.extend_from_slice(&position.generation.to_le_bytes());
    out.extend_from_slice(&position.offset.to_le_bytes());
    out
}

/// A position in another encoding, such as bincode's before #152, reads as
/// index version 0, which no index has, so the index is rebuilt.
fn decode_position(bytes: &[u8]) -> JournalPosition {
    let field = |at: usize| u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap());
    match bytes.len() {
        36 => JournalPosition {
            index_version: u32::from_le_bytes(bytes[..4].try_into().unwrap()),
            store_id: uuid::Uuid::from_bytes(bytes[4..20].try_into().unwrap()),
            generation: field(20),
            offset: field(28),
        },
        _ => JournalPosition {
            index_version: 0,
            store_id: uuid::Uuid::nil(),
            generation: 0,
            offset: 0,
        },
    }
}

/// A scrub time as milliseconds since the Unix epoch, 0 meaning none.
fn scrub_millis(time: Option<SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn scrub_time(millis: u64) -> Option<SystemTime> {
    (millis != 0).then(|| UNIX_EPOCH + Duration::from_millis(millis))
}

/// A segment's `scrub_schedule` record: its light and deep scrub times in
/// milliseconds (each a little-endian u64), then, when a scrub found damage in it
/// (#195), 10 more bytes: which kind of scrub found it (1 light, 2 deep),
/// what it found (1 metadata mismatch, 2 content corrupted, 3 MAC
/// mismatch), and for a metadata mismatch the expected and actual lengths,
/// each a little-endian u32. A record without them is a segment with none.
fn encode_scrub_times(t: &ScrubTimes) -> Result<Vec<u8>> {
    let mut out = scrub_millis(t.light).to_le_bytes().to_vec();
    out.extend_from_slice(&scrub_millis(t.deep).to_le_bytes());
    if let Some(damage) = t.damage {
        out.push(match damage.found_by {
            ScrubKind::Light => 1,
            ScrubKind::Deep => 2,
        });
        let (what, expected, actual) = match damage.what {
            DamageKind::MetadataMismatch {
                expected_len,
                actual_len,
            } => (1, expected_len, actual_len),
            DamageKind::ContentCorrupted => (2, 0, 0),
            DamageKind::MacMismatch => (3, 0, 0),
        };
        out.push(what);
        out.extend_from_slice(&expected.to_le_bytes());
        out.extend_from_slice(&actual.to_le_bytes());
    }
    Ok(out)
}

fn decode_scrub_times(bytes: &[u8]) -> Result<ScrubTimes> {
    let (times, rest) = match bytes.len() {
        16 | 26 => bytes.split_at(16),
        n => bail!("a scrub schedule record is {n} bytes, not 16 or 26"),
    };
    let light = decode_u64(&times[..8], "a scrub time")?;
    let deep = decode_u64(&times[8..], "a scrub time")?;
    let damage = match rest {
        [] => None,
        [found_by, what, lens @ ..] => {
            let found_by = match found_by {
                1 => ScrubKind::Light,
                2 => ScrubKind::Deep,
                n => bail!("unknown scrub kind {n} in the scrub schedule"),
            };
            let len = |at: usize| u32::from_le_bytes(lens[at..at + 4].try_into().unwrap());
            let what = match what {
                1 => DamageKind::MetadataMismatch {
                    expected_len: len(0),
                    actual_len: len(4),
                },
                2 => DamageKind::ContentCorrupted,
                3 => DamageKind::MacMismatch,
                n => bail!("unknown damage {n} in the scrub schedule"),
            };
            Some(Damage { found_by, what })
        }
        _ => unreachable!(),
    };
    Ok(ScrubTimes {
        light: scrub_time(light),
        deep: scrub_time(deep),
        damage,
    })
}

/// The database at `path` is open in another process, or elsewhere in this
/// one. redb allows one open per file (#244).
#[derive(Debug)]
pub struct StoreInUse(pub PathBuf);

impl std::fmt::Display for StoreInUse {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "'{}' is open in another process", self.0.display())
    }
}

impl std::error::Error for StoreInUse {}

impl RedbStore {
    /// Open the database at `path`, creating it if needed. A database
    /// another process has open is refused at once with [`StoreInUse`]:
    /// redb takes and drops its lock with the file, so a store closed a
    /// moment ago opens again without waiting.
    pub fn open(path: &Path) -> Result<Self> {
        let db = Database::create(path).map_err(|err| match err {
            DatabaseError::DatabaseAlreadyOpen => {
                anyhow::Error::new(StoreInUse(path.to_path_buf()))
            }
            other => anyhow::Error::new(other).context(format!(
                "can't open the registry index '{}'",
                path.display()
            )),
        })?;
        let store = Self { db };
        // Every table exists from the start, so a read never finds one
        // missing, and the segment counter starts at 0.
        store.write(Durability::Immediate, |txn| {
            for table in INDEX_TABLES {
                txn.open_table(table)?;
            }
            txn.open_table(SCRUB_SCHEDULE)?;
            let mut counters = txn.open_table(COUNTERS)?;
            if counters.get(NEXT_SEGMENT_KEY)?.is_none() {
                counters.insert(NEXT_SEGMENT_KEY, &0_u64.to_le_bytes()[..])?;
            }
            Ok(())
        })?;
        Ok(store)
    }

    /// Run `f` in a write transaction and commit it with `durability`. An
    /// error from `f` aborts the transaction, leaving nothing changed.
    ///
    /// The index is a copy of the journal, so applying a record commits
    /// with [`Durability::None`]: a crash loses those commits and the
    /// journal replays them on the next open, since the journal position
    /// goes in the same transaction. What isn't journal state, such as the
    /// segment counter and the scrub schedule, commits with
    /// [`Durability::Immediate`].
    fn write<T>(
        &self,
        durability: Durability,
        f: impl FnOnce(&WriteTransaction) -> Result<T>,
    ) -> Result<T> {
        let mut txn = self.db.begin_write()?;
        txn.set_durability(durability)?;
        let out = f(&txn)?;
        txn.commit()?;
        Ok(out)
    }

    /// The value at `key` in `table`.
    fn get(&self, table: Def, key: &[u8]) -> Result<Option<Vec<u8>>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(table)?;
        get(&table, key)
    }

    /// Every entry of `table`, in key order.
    fn entries(&self, table: Def) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in table.iter()? {
            let (k, v) = entry?;
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }

    /// Up to `limit` entries of `table` from `start` on whose keys start
    /// with `prefix`, in key order.
    fn scan(
        &self,
        table: Def,
        start: &[u8],
        prefix: &[u8],
        limit: usize,
    ) -> Result<Vec<(Vec<u8>, Vec<u8>)>> {
        let txn = self.db.begin_read()?;
        let table = txn.open_table(table)?;
        let mut out = Vec::new();
        for entry in table.range(start..)? {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry?;
            if !k.value().starts_with(prefix) {
                break;
            }
            out.push((k.value().to_vec(), v.value().to_vec()));
        }
        Ok(out)
    }
}

/// The value at `key` in `table`, copied out.
fn get(
    table: &impl ReadableTable<&'static [u8], &'static [u8]>,
    key: &[u8],
) -> Result<Option<Vec<u8>>> {
    Ok(table.get(key)?.map(|v| v.value().to_vec()))
}

/// Remove `key` from `table`, returning what it held.
fn take(table: &mut Tbl<'_>, key: &[u8]) -> Result<Option<Vec<u8>>> {
    Ok(table.remove(key)?.map(|v| v.value().to_vec()))
}

impl MetadataStore for RedbStore {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>> {
        self.get(CAPSULES, &capsule_key(*id))?
            .map(|bytes| decode_capsule(&bytes))
            .transpose()
    }

    fn list_capsules(&self, limit: usize, start_after: Option<CapsuleId>) -> Result<Vec<Capsule>> {
        let start = match start_after {
            Some(id) => {
                let mut key = capsule_key(id).to_vec();
                key.push(0);
                key
            }
            None => Vec::new(),
        };
        self.scan(CAPSULES, &start, &[], limit)?
            .iter()
            .map(|(_, value)| decode_capsule(value))
            .collect()
    }

    fn add_deduped_bytes(&self, id: &CapsuleId, bytes: u64) -> Result<()> {
        self.write(Durability::Immediate, |txn| {
            let mut capsules = txn.open_table(CAPSULES)?;
            let key = capsule_key(*id);
            // No-op when the capsule is missing (e.g., a dedup update races
            // a deletion in tests).
            if let Some(data) = get(&capsules, &key)? {
                let mut capsule = decode_capsule(&data)?;
                capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
                capsules.insert(&key[..], &encode_capsule(&capsule)?[..])?;
            }
            Ok(())
        })
    }

    fn allocate_segment_id(&self) -> Result<SegmentId> {
        self.write(Durability::Immediate, |txn| {
            let mut counters = txn.open_table(COUNTERS)?;
            let next = match get(&counters, NEXT_SEGMENT_KEY)? {
                Some(v) => decode_u64(&v, "the segment counter")?,
                None => 0,
            };
            counters.insert(NEXT_SEGMENT_KEY, &(next + 1).to_le_bytes()[..])?;
            Ok(SegmentId(next))
        })
    }

    fn ensure_segment_floor(&self, floor: u64) -> Result<()> {
        self.write(Durability::Immediate, |txn| {
            let mut counters = txn.open_table(COUNTERS)?;
            let current = match get(&counters, NEXT_SEGMENT_KEY)? {
                Some(v) => decode_u64(&v, "the segment counter")?,
                None => 0,
            };
            if floor > current {
                counters.insert(NEXT_SEGMENT_KEY, &floor.to_le_bytes()[..])?;
            }
            Ok(())
        })
    }

    fn apply_journal(&self, record: &JournalRecord, position: &JournalPosition) -> Result<()> {
        let position = encode_position(position);
        self.write(Durability::None, |txn| {
            let mut trees = IndexTrees {
                capsules: txn.open_table(CAPSULES)?,
                content: txn.open_table(CONTENT)?,
                refs: txn.open_table(REFS)?,
                indexed: txn.open_table(INDEXED)?,
                candidates: txn.open_table(CANDIDATES)?,
                deduped: txn.open_table(DEDUPED)?,
                namespaces: txn.open_table(NAMESPACES)?,
                names: txn.open_table(NAMES)?,
                capsule_names: txn.open_table(CAPSULE_NAMES)?,
            };
            apply_record(record, &mut trees)?;
            txn.open_table(COUNTERS)?
                .insert(JOURNAL_POSITION_KEY, &position[..])?;
            Ok(())
        })
    }

    fn set_journal_position(&self, position: &JournalPosition) -> Result<()> {
        let position = encode_position(position);
        self.write(Durability::Immediate, |txn| {
            txn.open_table(COUNTERS)?
                .insert(JOURNAL_POSITION_KEY, &position[..])?;
            Ok(())
        })
    }

    fn journal_position(&self) -> Result<Option<JournalPosition>> {
        Ok(self
            .get(COUNTERS, JOURNAL_POSITION_KEY)?
            .map(|v| decode_position(&v)))
    }

    fn clear_index(&self) -> Result<()> {
        self.write(Durability::Immediate, |txn| {
            for table in INDEX_TABLES {
                txn.open_table(table)?.retain(|_, _| false)?;
            }
            txn.open_table(COUNTERS)?.remove(JOURNAL_POSITION_KEY)?;
            Ok(())
        })
    }

    fn has_records(&self) -> Result<bool> {
        let txn = self.db.begin_read()?;
        Ok(!txn.open_table(CAPSULES)?.is_empty()?)
    }

    fn segment_refs(&self) -> Result<HashMap<SegmentId, u32>> {
        self.entries(REFS)?
            .iter()
            .map(|(key, value)| {
                Ok((
                    decode_segment(key)?,
                    decode_u32(value, "a reference count")?,
                ))
            })
            .collect()
    }

    fn scrub_times(&self) -> Result<Vec<(SegmentId, ScrubTimes)>> {
        self.entries(SCRUB_SCHEDULE)?
            .iter()
            .map(|(key, value)| Ok((decode_segment(key)?, decode_scrub_times(value)?)))
            .collect()
    }

    fn put_scrub_times(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> Result<()> {
        self.write(Durability::Immediate, |txn| {
            let mut schedule = txn.open_table(SCRUB_SCHEDULE)?;
            for (segment, times) in changes {
                let key = segment_key(*segment);
                match times {
                    Some(t) => {
                        schedule.insert(&key[..], &encode_scrub_times(t)?[..])?;
                    }
                    None => {
                        schedule.remove(&key[..])?;
                    }
                }
            }
            Ok(())
        })
    }

    // Garbage collection deletes the segment once this returns, so the
    // removal is made durable first: a crash can't bring back an index
    // entry for a segment that's gone.
    fn deregister_unreferenced(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        let seg_key = segment_key(segment);
        let hash_key = hash_key(hash);
        self.write(Durability::Immediate, |txn| {
            if txn.open_table(REFS)?.get(&seg_key[..])?.is_some() {
                return Ok(false);
            }
            let mut content = txn.open_table(CONTENT)?;
            let mut candidates = txn.open_table(CANDIDATES)?;
            let named = get(&content, &hash_key)?.as_deref() == Some(&seg_key[..]);
            txn.open_table(INDEXED)?.remove(&seg_key[..])?;
            withdraw_candidate(&mut content, &mut candidates, &hash_key, segment)?;
            Ok(named)
        })
    }

    fn get_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>> {
        self.get(CONTENT, &hash_key(hash))?
            .map(|bytes| decode_segment(&bytes))
            .transpose()
    }

    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>> {
        self.entries(CONTENT)?
            .iter()
            .map(|(key, value)| Ok((decode_hash(key)?, decode_segment(value)?)))
            .collect()
    }

    fn get_namespace(&self, ns: &str) -> Result<Option<u64>> {
        self.get(NAMESPACES, ns.as_bytes())?
            .map(|v| decode_u64(&v, "a namespace's creation time"))
            .transpose()
    }

    fn list_namespaces(
        &self,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, u64)>> {
        let scan = prefix.as_bytes().to_vec();
        let start = match start_after {
            Some(after) => {
                // The smallest namespace that sorts after `after`.
                let mut after = after.as_bytes().to_vec();
                after.push(0);
                after.max(scan.clone())
            }
            None => scan.clone(),
        };
        self.scan(NAMESPACES, &start, &scan, limit)?
            .into_iter()
            .map(|(k, v)| {
                let ns = String::from_utf8(k).context("namespace is not UTF-8")?;
                Ok((ns, decode_u64(&v, "a namespace's creation time")?))
            })
            .collect()
    }

    fn names_of(&self, capsule: CapsuleId) -> Result<Vec<(String, String)>> {
        let prefix = capsule.as_uuid().as_bytes();
        self.scan(CAPSULE_NAMES, prefix, prefix, usize::MAX)?
            .iter()
            .map(|(key, _)| {
                let key = std::str::from_utf8(&key[16..]).context("name is not UTF-8")?;
                let (ns, name) = key
                    .split_once('\0')
                    .ok_or_else(|| anyhow!("a name index key has no namespace"))?;
                Ok((ns.to_string(), name.to_string()))
            })
            .collect()
    }

    fn get_name(&self, ns: &str, name: &str) -> Result<Option<Binding>> {
        self.get(NAMES, &name_key(ns, name))?
            .map(|v| decode_binding(&v))
            .transpose()
    }

    fn list_names(
        &self,
        ns: &str,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Binding)>> {
        let scan = name_key(ns, prefix);
        let start = match start_after {
            Some(after) => {
                // The smallest key that sorts after `after`.
                let mut after = name_key(ns, after);
                after.push(0);
                after.max(scan.clone())
            }
            None => scan.clone(),
        };
        self.scan(NAMES, &start, &scan, limit)?
            .iter()
            .map(|(k, v)| {
                let name = std::str::from_utf8(&k[ns.len() + 1..])
                    .context("name is not UTF-8")?
                    .to_string();
                Ok((name, decode_binding(v)?))
            })
            .collect()
    }
}

/// The tables a journal record changes, inside one write transaction.
struct IndexTrees<'txn> {
    capsules: Tbl<'txn>,
    content: Tbl<'txn>,
    refs: Tbl<'txn>,
    indexed: Tbl<'txn>,
    candidates: Tbl<'txn>,
    deduped: Tbl<'txn>,
    namespaces: Tbl<'txn>,
    names: Tbl<'txn>,
    capsule_names: Tbl<'txn>,
}

// The content index follows from the live capsules alone: each hash maps to
// the lowest numbered segment that holds it and that an entry in a live
// dedupe capsule references. It doesn't depend on the order capsules were
// written and dropped in, so an index rebuilt from a compacted journal,
// which holds only the live capsules, is the same index (#188). Before, a
// hash went to the first segment written with it and left the index when
// that segment was freed, even while another live segment held it.

fn read_candidates(candidates: &Tbl<'_>, hash_key: &[u8]) -> Result<Vec<SegmentId>> {
    match get(candidates, hash_key)? {
        Some(v) => decode_segments(&v),
        None => Ok(Vec::new()),
    }
}

/// Point `hash_key` at the lowest of `segments`, or at nothing.
fn write_candidates(
    content: &mut Tbl<'_>,
    candidates: &mut Tbl<'_>,
    hash_key: &[u8],
    segments: &[SegmentId],
) -> Result<()> {
    match segments.first() {
        Some(lowest) => {
            content.insert(hash_key, &segment_key(*lowest)[..])?;
            candidates.insert(hash_key, &encode_segments(segments)[..])?;
        }
        None => {
            content.remove(hash_key)?;
            candidates.remove(hash_key)?;
        }
    }
    Ok(())
}

/// Stop offering `segment` for `hash_key`.
fn withdraw_candidate(
    content: &mut Tbl<'_>,
    candidates: &mut Tbl<'_>,
    hash_key: &[u8],
    segment: SegmentId,
) -> Result<()> {
    let mut segments = read_candidates(candidates, hash_key)?;
    let before = segments.len();
    segments.retain(|s| *s != segment);
    if segments.len() != before {
        write_candidates(content, candidates, hash_key, &segments)?;
    }
    Ok(())
}

/// Count an entry in a dedupe capsule that references `segment`, which
/// holds `hash`. Its first such entry offers it for dedup.
fn add_dedupe_ref(
    t: &mut IndexTrees<'_>,
    segment: SegmentId,
    seg_key: &[u8],
    hash: ContentHash,
) -> Result<()> {
    let (hash, count): (ContentHash, u32) = match get(&t.indexed, seg_key)? {
        Some(v) => decode_dedupe_refs(&v)?,
        None => (hash, 0),
    };
    t.indexed.insert(
        seg_key,
        &encode_dedupe_refs(&hash, count.saturating_add(1))[..],
    )?;
    if count == 0 {
        let hash_key = hash_key(&hash);
        let mut segments = read_candidates(&t.candidates, &hash_key)?;
        if let Err(at) = segments.binary_search(&segment) {
            segments.insert(at, segment);
        }
        write_candidates(&mut t.content, &mut t.candidates, &hash_key, &segments)?;
    }
    Ok(())
}

/// Undo [`add_dedupe_ref`] for an entry of a dropped dedupe capsule. When no
/// live dedupe capsule references `segment` any more, it's no longer
/// offered, and its hash moves to the next segment that holds it, if any.
fn remove_dedupe_ref(t: &mut IndexTrees<'_>, segment: SegmentId, seg_key: &[u8]) -> Result<()> {
    let Some(v) = get(&t.indexed, seg_key)? else {
        return Ok(());
    };
    let (hash, count) = decode_dedupe_refs(&v)?;
    if count > 1 {
        t.indexed
            .insert(seg_key, &encode_dedupe_refs(&hash, count - 1)[..])?;
        return Ok(());
    }
    t.indexed.remove(seg_key)?;
    withdraw_candidate(&mut t.content, &mut t.candidates, &hash_key(&hash), segment)
}

/// Remove a name from under the capsule it points at, if it's bound.
fn unindex_name(t: &mut IndexTrees<'_>, key: &[u8]) -> Result<()> {
    if let Some(old) = get(&t.names, key)? {
        let old = decode_binding(&old)?;
        t.capsule_names
            .remove(&capsule_name_key(old.capsule_id, key)[..])?;
    }
    Ok(())
}

fn read_count(refs: &Tbl<'_>, key: &[u8]) -> Result<u32> {
    match get(refs, key)? {
        Some(v) => decode_u32(&v, "a reference count"),
        None => Ok(0),
    }
}

/// Apply a journal record's effects (`docs/on-disk-format.md` §7). Replaying
/// a record that's already applied changes nothing: a `CAPSULE` for a
/// capsule that exists, or a `DROP` for one that doesn't, is skipped, and
/// the name records set state rather than change it.
fn apply_record(record: &JournalRecord, t: &mut IndexTrees<'_>) -> Result<()> {
    match record {
        JournalRecord::Capsule(manifest) => {
            let key = capsule_key(manifest.capsule_id);
            if t.capsules.get(&key[..])?.is_some() {
                return Ok(());
            }
            t.capsules
                .insert(&key[..], &encode_capsule(&manifest.to_capsule())?[..])?;
            if manifest.dedupe {
                t.deduped.insert(&key[..], &[][..])?;
            }
            for entry in &manifest.entries {
                let seg_key = segment_key(entry.segment_id);
                let count = read_count(&t.refs, &seg_key)?;
                t.refs
                    .insert(&seg_key[..], &count.saturating_add(1).to_le_bytes()[..])?;
                // An all-zero hash means none (on-disk format §4). Only a
                // dedup hit on a segment stored without one carries it, and
                // such a segment is never offered, so it's never counted.
                if manifest.dedupe && entry.content_hash != [0u8; 32] {
                    let hash = ContentHash::from_bytes(&entry.content_hash);
                    add_dedupe_ref(t, entry.segment_id, &seg_key, hash)?;
                }
            }
        }
        JournalRecord::Drop(id) => {
            let key = capsule_key(*id);
            let Some(bytes) = take(&mut t.capsules, &key)? else {
                return Ok(());
            };
            let dedupe = take(&mut t.deduped, &key)?.is_some();
            let capsule = decode_capsule(&bytes)?;
            for segment in &capsule.segments {
                let seg_key = segment_key(*segment);
                if dedupe {
                    remove_dedupe_ref(t, *segment, &seg_key)?;
                }
                let count = read_count(&t.refs, &seg_key)?;
                if count > 1 {
                    t.refs
                        .insert(&seg_key[..], &(count - 1).to_le_bytes()[..])?;
                } else {
                    t.refs.remove(&seg_key[..])?;
                }
            }
        }
        JournalRecord::Namespace { ns, created_at } => {
            // A namespace keeps the time it was first created.
            if t.namespaces.get(ns.as_bytes())?.is_none() {
                t.namespaces
                    .insert(ns.as_bytes(), &created_at.to_le_bytes()[..])?;
            }
        }
        JournalRecord::DropNamespace { ns, .. } => {
            t.namespaces.remove(ns.as_bytes())?;
        }
        JournalRecord::Bind {
            ns,
            name,
            capsule_id,
            metadata,
        } => {
            let key = name_key(ns, name);
            unindex_name(t, &key)?;
            t.names
                .insert(&key[..], &encode_binding(*capsule_id, metadata)?[..])?;
            t.capsule_names
                .insert(&capsule_name_key(*capsule_id, &key)[..], &[][..])?;
        }
        JournalRecord::Unbind { ns, name } => {
            let key = name_key(ns, name);
            unindex_name(t, &key)?;
            t.names.remove(&key[..])?;
        }
        // The journal keeps the store's key itself.
        JournalRecord::Key(_) => {}
        JournalRecord::Batch(records) => {
            for record in records {
                apply_record(record, t)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use common::Policy;
    use test_support::TempDir;

    /// A scratch directory, and the path of a database inside it.
    fn scratch_db(name: &str) -> (TempDir, PathBuf) {
        let dir = TempDir::new(&format!("space_{name}"));
        let path = dir.join("db");
        (dir, path)
    }

    /// A database another handle has open is refused at once, and opens
    /// again as soon as that handle is dropped, with no wait for a lock to
    /// clear as sled needed (#246, #244).
    #[test]
    fn a_store_in_use_is_refused_and_reopens_once_closed() {
        let (_dir, path) = scratch_db("in_use");
        let first = RedbStore::open(&path).unwrap();
        let err = RedbStore::open(&path).err().expect("opened a store in use");
        assert!(err.is::<StoreInUse>(), "{err:#}");
        drop(first);
        let again = RedbStore::open(&path);
        assert!(again.is_ok(), "{:?}", again.err());
        drop(again);
    }

    /// Records applied with no durability are still there after a clean
    /// close, and each table is created on the first open.
    #[test]
    fn applied_records_survive_a_reopen() {
        let (_dir, path) = scratch_db("reopen");
        let id = sample_capsule().id;
        let position = JournalPosition {
            index_version: crate::journal::INDEX_VERSION,
            store_id: uuid::Uuid::from_u128(7),
            generation: 1,
            offset: 128,
        };
        {
            let store = RedbStore::open(&path).unwrap();
            assert!(!store.has_records().unwrap());
            assert_eq!(store.allocate_segment_id().unwrap(), SegmentId(0));
            store
                .apply_journal(
                    &JournalRecord::Bind {
                        ns: "s3:bkt".to_string(),
                        name: "k".to_string(),
                        capsule_id: id,
                        metadata: Vec::new(),
                    },
                    &position,
                )
                .unwrap();
        }
        let store = RedbStore::open(&path).unwrap();
        assert_eq!(store.journal_position().unwrap(), Some(position));
        assert_eq!(
            store.get_name("s3:bkt", "k").unwrap().unwrap().capsule_id,
            id
        );
        assert_eq!(
            store.names_of(id).unwrap(),
            [("s3:bkt".to_string(), "k".to_string())]
        );
        assert_eq!(store.allocate_segment_id().unwrap(), SegmentId(1));
        drop(store);
    }

    #[test]
    fn scrub_times_round_trip_and_survive_a_reopen() {
        let (_dir, path) = scratch_db("scrub_times");
        let light = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let deep = UNIX_EPOCH + Duration::from_millis(1_700_000_500_456);
        {
            let store = RedbStore::open(&path).unwrap();
            store
                .put_scrub_times(&[
                    (
                        SegmentId(7),
                        Some(ScrubTimes {
                            light: Some(light),
                            deep: Some(deep),
                            damage: None,
                        }),
                    ),
                    (
                        SegmentId(8),
                        Some(ScrubTimes {
                            light: Some(light),
                            deep: None,
                            damage: None,
                        }),
                    ),
                    (
                        SegmentId(9),
                        Some(ScrubTimes {
                            light: Some(light),
                            deep: None,
                            damage: None,
                        }),
                    ),
                ])
                .unwrap();
            store.put_scrub_times(&[(SegmentId(9), None)]).unwrap();
            // A rebuild keeps the schedule: segment ids are never reused.
            store.clear_index().unwrap();
        }
        let store = RedbStore::open(&path).unwrap();
        let mut times = store.scrub_times().unwrap();
        times.sort_by_key(|(id, _)| id.0);
        assert_eq!(
            times,
            vec![
                (
                    SegmentId(7),
                    ScrubTimes {
                        light: Some(light),
                        deep: Some(deep),
                        damage: None,
                    }
                ),
                (
                    SegmentId(8),
                    ScrubTimes {
                        light: Some(light),
                        deep: None,
                        damage: None,
                    }
                ),
            ]
        );
        drop(store);
    }

    #[test]
    fn scrub_schedule_records_keep_their_encoding() {
        let at = |ms: u64| Some(UNIX_EPOCH + Duration::from_millis(ms));
        let plain = ScrubTimes {
            light: at(0x0102),
            deep: None,
            damage: None,
        };
        let bytes = encode_scrub_times(&plain).unwrap();
        assert_eq!(to_hex(&bytes), "02010000000000000000000000000000");
        assert_eq!(decode_scrub_times(&bytes).unwrap(), plain);

        let damaged = ScrubTimes {
            light: at(1),
            deep: at(2),
            damage: Some(Damage {
                found_by: ScrubKind::Deep,
                what: DamageKind::MetadataMismatch {
                    expected_len: 9,
                    actual_len: 4,
                },
            }),
        };
        let bytes = encode_scrub_times(&damaged).unwrap();
        assert_eq!(
            to_hex(&bytes),
            "01000000000000000200000000000000020109000000040000 00".replace(' ', "")
        );
        assert_eq!(decode_scrub_times(&bytes).unwrap(), damaged);
        for what in [DamageKind::ContentCorrupted, DamageKind::MacMismatch] {
            let t = ScrubTimes {
                damage: Some(Damage {
                    found_by: ScrubKind::Light,
                    what,
                }),
                ..damaged
            };
            assert_eq!(
                decode_scrub_times(&encode_scrub_times(&t).unwrap()).unwrap(),
                t
            );
        }
        assert!(decode_scrub_times(&bytes[..20]).is_err());
    }

    fn to_hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn sample_capsule() -> Capsule {
        Capsule {
            id: CapsuleId::from_uuid(uuid::Uuid::from_u128(
                0x0011_2233_4455_6677_8899_aabb_ccdd_eeff,
            )),
            size: 4096,
            segments: vec![SegmentId(1), SegmentId(2)],
            created_at: 1_700_000_000,
            policy: Policy::default(),
            deduped_bytes: 7,
            digest: Some([0xAB; 32]),
        }
    }

    // Byte-for-byte encodings of the index records. space.db is an index of
    // the capsule journal, so a change here only needs a rebuild: bump
    // `journal::INDEX_VERSION` with it, and stores rebuild their index on open
    // (#113, #152).
    const CAPSULE_ID_HEX: &str = "00112233445566778899aabbccddeeff";
    const SEGMENT_ID_HEX: &str = "2a00000000000000";

    #[test]
    fn capsule_records_keep_their_on_disk_encoding() {
        let capsule = sample_capsule();
        assert_eq!(to_hex(&capsule_key(capsule.id)), CAPSULE_ID_HEX);
        let bytes = encode_capsule(&capsule).unwrap();
        assert_eq!(bytes, serde_json::to_vec(&capsule).unwrap());
        assert!(bytes.starts_with(b"{\"id\":\"00112233-4455-6677-8899-aabbccddeeff\","));
        let decoded = decode_capsule(&bytes).unwrap();
        assert_eq!(decoded.id, capsule.id);
        assert_eq!(decoded.size, capsule.size);
        assert_eq!(decoded.segments, capsule.segments);
        assert_eq!(decoded.created_at, capsule.created_at);
        assert_eq!(decoded.deduped_bytes, capsule.deduped_bytes);
        assert_eq!(decoded.digest, capsule.digest);
        assert_eq!(
            format!("{:?}", decoded.policy),
            format!("{:?}", capsule.policy)
        );
    }

    #[test]
    fn name_keys_keep_their_on_disk_encoding() {
        assert_eq!(name_key("s3:demo", "dir/a.txt"), b"s3:demo\0dir/a.txt");
        assert_eq!(name_key("s3:demo", ""), b"s3:demo\0");
        let id = CapsuleId::from_uuid(uuid::Uuid::from_u128(0x0102));
        let mut expected = id.as_uuid().as_bytes().to_vec();
        expected.extend_from_slice(b"s3:demo\0dir/a.txt");
        assert_eq!(
            capsule_name_key(id, &name_key("s3:demo", "dir/a.txt")),
            expected
        );

        let id = sample_capsule().id;
        let metadata = vec![("a".to_string(), "bc".to_string())];
        let value = encode_binding(id, &metadata).unwrap();
        assert_eq!(
            to_hex(&value),
            format!("{CAPSULE_ID_HEX}{}", to_hex(br#"[["a","bc"]]"#))
        );
        let binding = decode_binding(&value).unwrap();
        assert_eq!((binding.capsule_id, binding.metadata), (id, metadata));
        assert!(decode_binding(&value[..15]).is_err());
    }

    #[test]
    fn content_and_counter_records_keep_their_on_disk_encoding() {
        let hash = ContentHash("ab".repeat(32));
        assert_eq!(hash_key(&hash), "ab".repeat(32).into_bytes());
        assert_eq!(decode_hash(&hash_key(&hash)).unwrap(), hash);
        assert_eq!(to_hex(&segment_key(SegmentId(42))), SEGMENT_ID_HEX);
        assert_eq!(
            decode_segment(&segment_key(SegmentId(42))).unwrap(),
            SegmentId(42)
        );
        assert!(decode_segment(&[0x2a]).is_err());

        let refs = encode_dedupe_refs(&hash, 3);
        assert_eq!(to_hex(&refs[..4]), "03000000");
        assert_eq!(decode_dedupe_refs(&refs).unwrap(), (hash, 3));
        let segments = [SegmentId(42), SegmentId(1)];
        assert_eq!(
            to_hex(&encode_segments(&segments)),
            format!("{SEGMENT_ID_HEX}0100000000000000")
        );
        assert_eq!(
            decode_segments(&encode_segments(&segments)).unwrap(),
            segments
        );
        assert!(decode_segments(&[0; 9]).is_err());
    }

    /// The segment counter, reference counts and scrub times read the same as
    /// bincode wrote them, so the scrub schedule survives the move off it
    /// (#152).
    #[test]
    fn records_that_outlive_a_rebuild_read_as_bincode_wrote_them() {
        // What bincode 1.3 wrote for 42u64, 3u32 and (light, deep) millis.
        assert_eq!(decode_u64(&[42, 0, 0, 0, 0, 0, 0, 0], "").unwrap(), 42);
        assert_eq!(decode_u32(&[3, 0, 0, 0], "").unwrap(), 3);
        let mut times = 5_u64.to_le_bytes().to_vec();
        times.extend_from_slice(&6_u64.to_le_bytes());
        let t = decode_scrub_times(&times).unwrap();
        assert_eq!(scrub_millis(t.light), 5);
        assert_eq!(scrub_millis(t.deep), 6);
        assert_eq!(encode_scrub_times(&t).unwrap(), times);
    }

    #[test]
    fn journal_positions_round_trip_and_old_ones_force_a_rebuild() {
        let position = JournalPosition {
            index_version: crate::journal::INDEX_VERSION,
            store_id: uuid::Uuid::from_u128(0x0011_2233_4455_6677_8899_aabb_ccdd_eeff),
            generation: 2,
            offset: 4096,
        };
        let bytes = encode_position(&position);
        assert_eq!(bytes.len(), 36);
        assert_eq!(decode_position(&bytes), position);
        // bincode's encoding, 44 bytes: the version, the store id with an
        // 8-byte length, the generation and the offset.
        let mut old = 5_u32.to_le_bytes().to_vec();
        old.extend_from_slice(&16_u64.to_le_bytes());
        old.extend_from_slice(position.store_id.as_bytes());
        old.extend_from_slice(&2_u64.to_le_bytes());
        old.extend_from_slice(&4096_u64.to_le_bytes());
        assert_eq!(decode_position(&old).index_version, 0);
    }
}
