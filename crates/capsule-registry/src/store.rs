use anyhow::{anyhow, bail, Context, Result};
use common::scrub::{Damage, DamageKind, ScrubKind, ScrubTimes};
use common::{Capsule, CapsuleId, ContentHash, SegmentId};
use sled::transaction::{
    ConflictableTransactionError, ConflictableTransactionResult, TransactionError,
    TransactionalTree,
};
use sled::{Db, Transactional};
use std::collections::HashMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::warn;

use crate::journal::JournalPosition;
use common::format::JournalRecord;

const NEXT_SEGMENT_KEY: &[u8] = b"next_segment_id";
/// Key in the counters tree for the journal position the index has reached.
const JOURNAL_POSITION_KEY: &[u8] = b"journal_position";

/// Abstraction over the metadata storage backend.
/// Allows swapping Sled for other engines (e.g., RocksDB, in-memory) without
/// changing the registry surface.
/// What a name points at: a capsule, and the metadata kept with the name
/// (`docs/on-disk-format.md` §5.6).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binding {
    pub capsule_id: CapsuleId,
    pub metadata: Vec<(String, String)>,
}

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

/// Sled-backed metadata store: crash-safe, concurrent, embeddable.
pub struct SledStore {
    db: Db,
    capsules: sled::Tree,
    /// Content hash to the segment dedup reuses for it: the lowest numbered
    /// of its `candidates` (#188).
    content: sled::Tree,
    /// Segment id to the number of capsule entries that reference it.
    refs: sled::Tree,
    /// Segment id to its content hash and the number of entries in live
    /// dedupe capsules that reference it, for segments with any.
    indexed: sled::Tree,
    /// Content hash to every segment that holds it and that a live dedupe
    /// capsule references, lowest first.
    candidates: sled::Tree,
    /// The live capsules written with dedupe on.
    deduped: sled::Tree,
    counters: sled::Tree,
    /// Namespace to its creation time.
    namespaces: sled::Tree,
    /// Namespace and name ([`name_key`]) to the capsule the name points at.
    names: sled::Tree,
    /// Each name under the capsule it points at ([`capsule_name_key`]), so
    /// deleting a capsule can unbind its names (#138).
    capsule_names: sled::Tree,
    /// Segment id to when it was last light- and deep-scrubbed.
    scrub_schedule: sled::Tree,
}

/// Trees that held S3 buckets and keys before names moved to the journal.
const LEGACY_NAME_TREES: [&str; 2] = ["s3_buckets", "s3_objects"];

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

impl SledStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = sled::open(path)?;
        let capsules = db.open_tree("capsules")?;
        let content = db.open_tree("content")?;
        let refs = db.open_tree("refs")?;
        let indexed = db.open_tree("indexed")?;
        let candidates = db.open_tree("candidates")?;
        let deduped = db.open_tree("deduped")?;
        let counters = db.open_tree("counters")?;
        let namespaces = db.open_tree("namespaces")?;
        let names = db.open_tree("names")?;
        let capsule_names = db.open_tree("capsule_names")?;
        let scrub_schedule = db.open_tree("scrub_schedule")?;

        // Initialize the segment counter if it doesn't exist yet.
        let _ = counters.compare_and_swap(
            NEXT_SEGMENT_KEY,
            None as Option<&[u8]>,
            Some(&0_u64.to_le_bytes()[..]),
        )?;

        Ok(Self {
            db,
            capsules,
            content,
            refs,
            indexed,
            candidates,
            deduped,
            counters,
            namespaces,
            names,
            capsule_names,
            scrub_schedule,
        })
    }

    fn flush(&self) -> Result<()> {
        self.db.flush()?;
        Ok(())
    }

    /// How many entries the legacy S3 trees hold. Checked without opening a
    /// tree that isn't there, since opening one creates it.
    fn legacy_name_entries(&self) -> Result<usize> {
        let existing = self.db.tree_names();
        let mut entries = 0;
        for tree in LEGACY_NAME_TREES {
            if existing.iter().any(|name| name.as_ref() == tree.as_bytes()) {
                entries += self.db.open_tree(tree)?.len();
            }
        }
        Ok(entries)
    }
}

impl MetadataStore for SledStore {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>> {
        match self.capsules.get(capsule_key(*id))? {
            Some(bytes) => Ok(Some(decode_capsule(&bytes)?)),
            None => Ok(None),
        }
    }

    fn list_capsules(&self, limit: usize, start_after: Option<CapsuleId>) -> Result<Vec<Capsule>> {
        let mut capsules = Vec::with_capacity(limit);

        let start_key = match start_after {
            Some(id) => {
                let mut key = capsule_key(id).to_vec();
                key.push(0);
                key
            }
            None => Vec::new(),
        };

        let iter = self.capsules.range(start_key..);

        for entry in iter.take(limit) {
            let (_, value) = entry?;
            capsules.push(decode_capsule(&value)?);
        }

        Ok(capsules)
    }

    fn add_deduped_bytes(&self, id: &CapsuleId, bytes: u64) -> Result<()> {
        let updated =
            self.capsules
                .fetch_and_update(capsule_key(*id), |existing| -> Option<Vec<u8>> {
                    let data = existing?;
                    let mut capsule: Capsule = decode_capsule(data).ok()?;
                    capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
                    encode_capsule(&capsule).ok()
                })?;

        if updated.is_none() {
            // No-op when capsule is missing (e.g., dedup update races deletion in tests).
            return Ok(());
        }

        self.flush()?;
        Ok(())
    }

    fn allocate_segment_id(&self) -> Result<SegmentId> {
        let next = self
            .counters
            .fetch_and_update(NEXT_SEGMENT_KEY, |existing| -> Option<Vec<u8>> {
                let current = existing
                    .and_then(|ivec| decode_u64(ivec, "the segment counter").ok())
                    .unwrap_or(0);
                Some((current + 1).to_le_bytes().to_vec())
            })?
            .map(|ivec| decode_u64(&ivec, "the segment counter"))
            .transpose()?
            .unwrap_or(0);

        self.flush()?;
        Ok(SegmentId(next))
    }

    fn ensure_segment_floor(&self, floor: u64) -> Result<()> {
        self.counters
            .fetch_and_update(NEXT_SEGMENT_KEY, |existing| -> Option<Vec<u8>> {
                let current = existing
                    .and_then(|ivec| decode_u64(ivec, "the segment counter").ok())
                    .unwrap_or(0);
                Some(current.max(floor).to_le_bytes().to_vec())
            })?;
        Ok(())
    }

    fn apply_journal(&self, record: &JournalRecord, position: &JournalPosition) -> Result<()> {
        let position = encode_position(position);
        // More trees than sled's tuples take, so a slice.
        let trees: [&sled::Tree; 10] = [
            &self.capsules,
            &self.content,
            &self.refs,
            &self.indexed,
            &self.candidates,
            &self.deduped,
            &self.counters,
            &self.namespaces,
            &self.names,
            &self.capsule_names,
        ];
        let result = trees[..].transaction(|view| {
            let [capsules, content, refs, indexed, candidates, deduped, counters, namespaces, names, capsule_names] =
                &view[..]
            else {
                unreachable!("one view per tree");
            };
            let trees = IndexTrees {
                capsules,
                content,
                refs,
                indexed,
                candidates,
                deduped,
                namespaces,
                names,
                capsule_names,
            };
            apply_record(record, &trees)?;
            counters.insert(JOURNAL_POSITION_KEY, position.clone())?;
            Ok(())
        });
        match result {
            Ok(()) => Ok(()),
            Err(TransactionError::Abort(err)) => Err(err),
            Err(TransactionError::Storage(err)) => Err(err.into()),
        }
    }

    fn set_journal_position(&self, position: &JournalPosition) -> Result<()> {
        self.counters
            .insert(JOURNAL_POSITION_KEY, encode_position(position))?;
        Ok(())
    }

    fn journal_position(&self) -> Result<Option<JournalPosition>> {
        Ok(self
            .counters
            .get(JOURNAL_POSITION_KEY)?
            .map(|v| decode_position(&v)))
    }

    fn clear_index(&self) -> Result<()> {
        self.capsules.clear()?;
        self.content.clear()?;
        self.refs.clear()?;
        self.indexed.clear()?;
        self.candidates.clear()?;
        self.deduped.clear()?;
        self.namespaces.clear()?;
        self.names.clear()?;
        self.capsule_names.clear()?;
        self.counters.remove(JOURNAL_POSITION_KEY)?;
        // Names written before the journal held them aren't in it, so a
        // rebuild can't keep them.
        let legacy = self.legacy_name_entries()?;
        if legacy > 0 {
            warn!(
                entries = legacy,
                "dropping S3 buckets and keys recorded before names moved to the capsule journal"
            );
        }
        for tree in LEGACY_NAME_TREES {
            self.db.drop_tree(tree)?;
        }
        self.flush()
    }

    fn has_records(&self) -> Result<bool> {
        Ok(!self.capsules.is_empty() || self.legacy_name_entries()? > 0)
    }

    fn segment_refs(&self) -> Result<HashMap<SegmentId, u32>> {
        let mut out = HashMap::new();
        for entry in self.refs.iter() {
            let (key, value) = entry?;
            out.insert(
                decode_segment(&key)?,
                decode_u32(&value, "a reference count")?,
            );
        }
        Ok(out)
    }

    fn scrub_times(&self) -> Result<Vec<(SegmentId, ScrubTimes)>> {
        let mut out = Vec::new();
        for entry in self.scrub_schedule.iter() {
            let (key, value) = entry?;
            out.push((decode_segment(&key)?, decode_scrub_times(&value)?));
        }
        Ok(out)
    }

    fn put_scrub_times(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> Result<()> {
        let mut batch = sled::Batch::default();
        for (segment, times) in changes {
            let key = segment_key(*segment).to_vec();
            match times {
                Some(t) => batch.insert(key, encode_scrub_times(t)?),
                None => batch.remove(key),
            }
        }
        self.scrub_schedule.apply_batch(batch)?;
        self.flush()
    }

    fn deregister_unreferenced(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        let seg_key = segment_key(segment);
        if self.refs.contains_key(seg_key)? {
            return Ok(false);
        }
        let hash_key = hash_key(hash);
        let result = (&self.content, &self.indexed, &self.candidates).transaction(
            |(content, indexed, candidates)| {
                let named = content.get(&hash_key)?.as_deref() == Some(&seg_key[..]);
                indexed.remove(&seg_key[..])?;
                withdraw_candidate(content, candidates, &hash_key, segment)?;
                Ok(named)
            },
        );
        match result {
            Ok(removed) => Ok(removed),
            Err(TransactionError::Abort(err)) => Err(err),
            Err(TransactionError::Storage(err)) => Err(err.into()),
        }
    }

    fn get_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>> {
        self.content
            .get(hash_key(hash))?
            .map(|bytes| decode_segment(&bytes))
            .transpose()
    }

    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>> {
        let mut entries = Vec::new();
        for entry in self.content.iter() {
            let (key, value) = entry?;
            entries.push((decode_hash(&key)?, decode_segment(&value)?));
        }
        Ok(entries)
    }

    fn get_namespace(&self, ns: &str) -> Result<Option<u64>> {
        self.namespaces
            .get(ns.as_bytes())?
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
        let mut out = Vec::new();
        for entry in self.namespaces.range(start..) {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry?;
            if !k.starts_with(&scan) {
                break;
            }
            let ns = String::from_utf8(k.to_vec()).context("namespace is not UTF-8")?;
            out.push((ns, decode_u64(&v, "a namespace's creation time")?));
        }
        Ok(out)
    }

    fn names_of(&self, capsule: CapsuleId) -> Result<Vec<(String, String)>> {
        let mut out = Vec::new();
        for entry in self.capsule_names.scan_prefix(capsule.as_uuid().as_bytes()) {
            let (key, _) = entry?;
            let key = std::str::from_utf8(&key[16..]).context("name is not UTF-8")?;
            let (ns, name) = key
                .split_once('\0')
                .ok_or_else(|| anyhow!("a name index key has no namespace"))?;
            out.push((ns.to_string(), name.to_string()));
        }
        Ok(out)
    }

    fn get_name(&self, ns: &str, name: &str) -> Result<Option<Binding>> {
        self.names
            .get(name_key(ns, name))?
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

        let mut out = Vec::new();
        for entry in self.names.range(start..) {
            if out.len() >= limit {
                break;
            }
            let (k, v) = entry?;
            if !k.starts_with(&scan) {
                break;
            }
            let name = std::str::from_utf8(&k[ns.len() + 1..])
                .context("name is not UTF-8")?
                .to_string();
            out.push((name, decode_binding(&v)?));
        }
        Ok(out)
    }
}

/// The trees a journal record changes, inside one transaction.
struct IndexTrees<'a> {
    capsules: &'a TransactionalTree,
    content: &'a TransactionalTree,
    refs: &'a TransactionalTree,
    indexed: &'a TransactionalTree,
    candidates: &'a TransactionalTree,
    deduped: &'a TransactionalTree,
    namespaces: &'a TransactionalTree,
    names: &'a TransactionalTree,
    capsule_names: &'a TransactionalTree,
}

// The content index follows from the live capsules alone: each hash maps to
// the lowest numbered segment that holds it and that an entry in a live
// dedupe capsule references. It doesn't depend on the order capsules were
// written and dropped in, so an index rebuilt from a compacted journal,
// which holds only the live capsules, is the same index (#188). Before, a
// hash went to the first segment written with it and left the index when
// that segment was freed, even while another live segment held it.

fn read_candidates(
    candidates: &TransactionalTree,
    hash_key: &[u8],
) -> ConflictableTransactionResult<Vec<SegmentId>, anyhow::Error> {
    match candidates.get(hash_key)? {
        Some(v) => decode_segments(&v).map_err(abort),
        None => Ok(Vec::new()),
    }
}

/// Point `hash_key` at the lowest of `segments`, or at nothing.
fn write_candidates(
    content: &TransactionalTree,
    candidates: &TransactionalTree,
    hash_key: &[u8],
    segments: &[SegmentId],
) -> ConflictableTransactionResult<(), anyhow::Error> {
    match segments.first() {
        Some(lowest) => {
            content.insert(hash_key, &segment_key(*lowest)[..])?;
            candidates.insert(hash_key, encode_segments(segments))?;
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
    content: &TransactionalTree,
    candidates: &TransactionalTree,
    hash_key: &[u8],
    segment: SegmentId,
) -> ConflictableTransactionResult<(), anyhow::Error> {
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
    t: &IndexTrees<'_>,
    segment: SegmentId,
    seg_key: &[u8],
    hash: ContentHash,
) -> ConflictableTransactionResult<(), anyhow::Error> {
    let (hash, count): (ContentHash, u32) = match t.indexed.get(seg_key)? {
        Some(v) => decode_dedupe_refs(&v).map_err(abort)?,
        None => (hash, 0),
    };
    t.indexed
        .insert(seg_key, encode_dedupe_refs(&hash, count.saturating_add(1)))?;
    if count == 0 {
        let hash_key = hash_key(&hash);
        let mut segments = read_candidates(t.candidates, &hash_key)?;
        if let Err(at) = segments.binary_search(&segment) {
            segments.insert(at, segment);
        }
        write_candidates(t.content, t.candidates, &hash_key, &segments)?;
    }
    Ok(())
}

/// Undo [`add_dedupe_ref`] for an entry of a dropped dedupe capsule. When no
/// live dedupe capsule references `segment` any more, it's no longer
/// offered, and its hash moves to the next segment that holds it, if any.
fn remove_dedupe_ref(
    t: &IndexTrees<'_>,
    segment: SegmentId,
    seg_key: &[u8],
) -> ConflictableTransactionResult<(), anyhow::Error> {
    let Some(v) = t.indexed.get(seg_key)? else {
        return Ok(());
    };
    let (hash, count) = decode_dedupe_refs(&v).map_err(abort)?;
    if count > 1 {
        t.indexed
            .insert(seg_key, encode_dedupe_refs(&hash, count - 1))?;
        return Ok(());
    }
    t.indexed.remove(seg_key)?;
    withdraw_candidate(t.content, t.candidates, &hash_key(&hash), segment)
}

/// Remove a name from under the capsule it points at, if it's bound.
fn unindex_name(
    t: &IndexTrees<'_>,
    key: &[u8],
) -> ConflictableTransactionResult<(), anyhow::Error> {
    if let Some(old) = t.names.get(key)? {
        let old = decode_binding(&old).map_err(abort)?;
        t.capsule_names
            .remove(capsule_name_key(old.capsule_id, key))?;
    }
    Ok(())
}

fn abort<E: std::fmt::Display>(err: E) -> ConflictableTransactionError<anyhow::Error> {
    ConflictableTransactionError::Abort(anyhow!("{err}"))
}

fn read_count(
    refs: &TransactionalTree,
    key: &[u8],
) -> ConflictableTransactionResult<u32, anyhow::Error> {
    match refs.get(key)? {
        Some(v) => decode_u32(&v, "a reference count").map_err(abort),
        None => Ok(0),
    }
}

/// Apply a journal record's effects (`docs/on-disk-format.md` §7). Replaying
/// a record that's already applied changes nothing: a `CAPSULE` for a
/// capsule that exists, or a `DROP` for one that doesn't, is skipped, and
/// the name records set state rather than change it.
fn apply_record(
    record: &JournalRecord,
    t: &IndexTrees<'_>,
) -> ConflictableTransactionResult<(), anyhow::Error> {
    match record {
        JournalRecord::Capsule(manifest) => {
            let key = capsule_key(manifest.capsule_id);
            if t.capsules.get(key)?.is_some() {
                return Ok(());
            }
            t.capsules.insert(
                &key[..],
                encode_capsule(&manifest.to_capsule()).map_err(abort)?,
            )?;
            if manifest.dedupe {
                t.deduped.insert(&key[..], &[])?;
            }
            for entry in &manifest.entries {
                let seg_key = segment_key(entry.segment_id);
                let count = read_count(t.refs, &seg_key)?;
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
            let Some(bytes) = t.capsules.remove(&key[..])? else {
                return Ok(());
            };
            let dedupe = t.deduped.remove(&key[..])?.is_some();
            let capsule = decode_capsule(&bytes).map_err(abort)?;
            for segment in &capsule.segments {
                let seg_key = segment_key(*segment);
                if dedupe {
                    remove_dedupe_ref(t, *segment, &seg_key)?;
                }
                let count = read_count(t.refs, &seg_key)?;
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
            t.names.insert(
                key.clone(),
                encode_binding(*capsule_id, metadata).map_err(abort)?,
            )?;
            t.capsule_names
                .insert(capsule_name_key(*capsule_id, &key), &[])?;
        }
        JournalRecord::Unbind { ns, name } => {
            let key = name_key(ns, name);
            unindex_name(t, &key)?;
            t.names.remove(key)?;
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

    #[test]
    fn scrub_times_round_trip_and_survive_a_reopen() {
        let dir = std::env::temp_dir().join(format!("space_scrub_times_{}", uuid::Uuid::new_v4()));
        let path = dir.to_str().unwrap().to_string();
        let light = UNIX_EPOCH + Duration::from_millis(1_700_000_000_123);
        let deep = UNIX_EPOCH + Duration::from_millis(1_700_000_500_456);
        {
            let store = SledStore::open(&path).unwrap();
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
        let store = SledStore::open(&path).unwrap();
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
        let _ = std::fs::remove_dir_all(&dir);
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
