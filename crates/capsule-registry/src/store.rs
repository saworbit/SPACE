use anyhow::{anyhow, Context, Result};
use common::scrub::ScrubTimes;
use common::{
    Capsule, CapsuleId, CompressionPolicy, ContentHash, CryptoProfile, EncryptionPolicy,
    FederationStrategy, LayoutPolicy, Policy, SegmentId,
};
use serde::{Deserialize, Serialize};
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
use std::io::{BufWriter, Cursor, Write};

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
    fn put_capsule(&self, capsule: &Capsule) -> Result<()>;
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
    fn put_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<()>;
    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>>;

    /// Names (`docs/on-disk-format.md` §5.5–§5.6). Each namespace maps to its
    /// creation time, and each name to the capsule it points at and its
    /// metadata. Only applying journal records changes them.
    fn get_namespace(&self, ns: &str) -> Result<Option<u64>>;
    fn get_name(&self, ns: &str, name: &str) -> Result<Option<Binding>>;
    /// Names in `ns` that start with `prefix` and sort after `start_after`,
    /// in byte order.
    fn list_names(
        &self,
        ns: &str,
        prefix: &str,
        start_after: Option<&str>,
        limit: usize,
    ) -> Result<Vec<(String, Binding)>>;

    /// Covers capsules, content and the segment counter, not names.
    #[allow(dead_code)]
    fn create_snapshot(&self) -> Result<Vec<u8>>;
    #[allow(dead_code)]
    fn restore_snapshot(&self, data: &[u8]) -> Result<()>;
}

/// Snapshot payload for metadata store checkpoints.
#[derive(Serialize, Deserialize)]
#[allow(dead_code)]
struct SnapshotHeader {
    next_segment_id: u64,
    capsule_entries: u64,
    content_entries: u64,
}

// ---------------------------------------------------------------------------
// Backwards-compatible decode helpers (bincode + evolving structs)
// ---------------------------------------------------------------------------
//
// `bincode` serializes structs as a fixed-order sequence, which means adding
// fields to `Policy` breaks decoding of previously persisted capsules.
// We keep a minimal "legacy" representation here so older registries can still
// be opened without manual migration.

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapsuleLegacy {
    id: CapsuleId,
    size: u64,
    segments: Vec<SegmentId>,
    created_at: u64,
    policy: PolicyLegacy,
    deduped_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CapsuleLegacyV2 {
    id: CapsuleId,
    size: u64,
    segments: Vec<SegmentId>,
    created_at: u64,
    policy: PolicyLegacyV2,
    deduped_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolicyLegacyV2 {
    pub compression: CompressionPolicy,
    pub dedupe: bool,
    pub compact_interval_secs: Option<u64>,
    pub erasure_profile: Option<String>,
    #[serde(default)]
    pub encryption: EncryptionPolicy,
    #[serde(default)]
    pub crypto_profile: CryptoProfile,
    #[serde(default)]
    pub layout: LayoutPolicy,
    #[serde(default)]
    pub federation: Option<FederationPolicyLegacy>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct FederationPolicyLegacy {
    pub strategy: FederationStrategy,
    pub target_zones: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct PolicyLegacy {
    pub compression: CompressionPolicy,
    pub dedupe: bool,
    pub compact_interval_secs: Option<u64>,
    pub erasure_profile: Option<String>,
    #[serde(default)]
    pub encryption: EncryptionPolicy,
    #[serde(default)]
    pub crypto_profile: CryptoProfile,
    #[serde(default)]
    pub layout: LayoutPolicy,
}

impl From<PolicyLegacy> for Policy {
    fn from(value: PolicyLegacy) -> Self {
        Policy {
            compression: value.compression,
            dedupe: value.dedupe,
            compact_interval_secs: value.compact_interval_secs,
            erasure_profile: value.erasure_profile,
            encryption: value.encryption,
            crypto_profile: value.crypto_profile,
            layout: value.layout,
            ..Policy::default()
        }
    }
}

impl From<PolicyLegacyV2> for Policy {
    fn from(value: PolicyLegacyV2) -> Self {
        let mut policy = Policy {
            compression: value.compression,
            dedupe: value.dedupe,
            compact_interval_secs: value.compact_interval_secs,
            erasure_profile: value.erasure_profile,
            encryption: value.encryption,
            crypto_profile: value.crypto_profile,
            layout: value.layout,
            ..Policy::default()
        };

        if let Some(fed) = value.federation {
            policy.federation.targets = fed.target_zones;
            policy.federation.strategy = Some(fed.strategy);
        }

        policy
    }
}

impl From<CapsuleLegacy> for Capsule {
    fn from(value: CapsuleLegacy) -> Self {
        Self {
            id: value.id,
            size: value.size,
            segments: value.segments,
            created_at: value.created_at,
            policy: value.policy.into(),
            deduped_bytes: value.deduped_bytes,
            digest: None,
        }
    }
}

impl From<CapsuleLegacyV2> for Capsule {
    fn from(value: CapsuleLegacyV2) -> Self {
        Self {
            id: value.id,
            size: value.size,
            segments: value.segments,
            created_at: value.created_at,
            policy: value.policy.into(),
            deduped_bytes: value.deduped_bytes,
            digest: None,
        }
    }
}

fn is_unexpected_eof(err: &bincode::Error) -> bool {
    matches!(
        &**err,
        bincode::ErrorKind::Io(io) if io.kind() == std::io::ErrorKind::UnexpectedEof
    )
}

fn decode_capsule_bytes(bytes: &[u8]) -> std::result::Result<Capsule, bincode::Error> {
    match bincode::deserialize::<Capsule>(bytes) {
        Ok(capsule) => Ok(capsule),
        Err(err) => {
            if let Ok(capsule) = bincode::deserialize::<CapsuleLegacyV2>(bytes).map(Into::into) {
                return Ok(capsule);
            }
            if is_unexpected_eof(&err) {
                return bincode::deserialize::<CapsuleLegacy>(bytes)
                    .map(Into::into)
                    .or(Err(err));
            }
            Err(err)
        }
    }
}

fn decode_capsule(bytes: &[u8]) -> Result<Capsule> {
    decode_capsule_bytes(bytes).map_err(|err| anyhow!(err))
}

/// Sled-backed metadata store: crash-safe, concurrent, embeddable.
pub struct SledStore {
    db: Db,
    capsules: sled::Tree,
    content: sled::Tree,
    /// Segment id to the number of capsule entries that reference it.
    refs: sled::Tree,
    /// Segment id to its content-index key, for segments the index holds.
    indexed: sled::Tree,
    counters: sled::Tree,
    /// Namespace to its creation time.
    namespaces: sled::Tree,
    /// Namespace and name ([`name_key`]) to the capsule the name points at.
    names: sled::Tree,
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

/// A scrub time as milliseconds since the Unix epoch, 0 meaning none.
fn scrub_millis(time: Option<SystemTime>) -> u64 {
    time.and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
}

fn scrub_time(millis: u64) -> Option<SystemTime> {
    (millis != 0).then(|| UNIX_EPOCH + Duration::from_millis(millis))
}

/// A name's index record: its capsule, then its metadata.
fn decode_binding(bytes: &[u8]) -> Result<Binding> {
    let (capsule_id, metadata) = bincode::deserialize(bytes)?;
    Ok(Binding {
        capsule_id,
        metadata,
    })
}

impl SledStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = sled::open(path)?;
        let capsules = db.open_tree("capsules")?;
        let content = db.open_tree("content")?;
        let refs = db.open_tree("refs")?;
        let indexed = db.open_tree("indexed")?;
        let counters = db.open_tree("counters")?;
        let namespaces = db.open_tree("namespaces")?;
        let names = db.open_tree("names")?;
        let scrub_schedule = db.open_tree("scrub_schedule")?;

        // Initialize the segment counter if it doesn't exist yet.
        let _ = counters.compare_and_swap(
            NEXT_SEGMENT_KEY,
            None as Option<&[u8]>,
            Some(bincode::serialize(&0_u64)?),
        )?;

        Ok(Self {
            db,
            capsules,
            content,
            refs,
            indexed,
            counters,
            namespaces,
            names,
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

    #[allow(dead_code)]
    fn current_segment_counter(&self) -> Result<u64> {
        self.counters
            .get(NEXT_SEGMENT_KEY)?
            .map(|ivec| bincode::deserialize::<u64>(&ivec))
            .transpose()?
            .ok_or_else(|| anyhow!("segment counter missing"))
    }
}

impl MetadataStore for SledStore {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>> {
        let key = bincode::serialize(id)?;
        match self.capsules.get(key)? {
            Some(bytes) => Ok(Some(decode_capsule(&bytes)?)),
            None => Ok(None),
        }
    }

    fn put_capsule(&self, capsule: &Capsule) -> Result<()> {
        let key = bincode::serialize(&capsule.id)?;
        let val = bincode::serialize(capsule)?;
        self.capsules.insert(key, val)?;
        self.flush()?;
        Ok(())
    }

    fn list_capsules(&self, limit: usize, start_after: Option<CapsuleId>) -> Result<Vec<Capsule>> {
        let mut capsules = Vec::with_capacity(limit);

        let start_key = match start_after {
            Some(id) => {
                let mut key = bincode::serialize(&id)?;
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
        let key = bincode::serialize(id)?;
        let updated = self
            .capsules
            .fetch_and_update(key, |existing| -> Option<Vec<u8>> {
                let data = existing?;
                let mut capsule: Capsule = decode_capsule_bytes(data).ok()?;
                capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
                bincode::serialize(&capsule).ok()
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
                let current: u64 = existing
                    .as_ref()
                    .and_then(|ivec| bincode::deserialize(ivec).ok())
                    .unwrap_or(0);
                let new_value = current + 1;
                bincode::serialize(&new_value).ok()
            })?
            .map(|ivec| bincode::deserialize::<u64>(&ivec))
            .transpose()?
            .unwrap_or(0);

        self.flush()?;
        Ok(SegmentId(next))
    }

    fn ensure_segment_floor(&self, floor: u64) -> Result<()> {
        self.counters
            .fetch_and_update(NEXT_SEGMENT_KEY, |existing| -> Option<Vec<u8>> {
                let current: u64 = existing
                    .and_then(|ivec| bincode::deserialize(ivec).ok())
                    .unwrap_or(0);
                bincode::serialize(&current.max(floor)).ok()
            })?;
        Ok(())
    }

    fn apply_journal(&self, record: &JournalRecord, position: &JournalPosition) -> Result<()> {
        let position = bincode::serialize(position)?;
        let result = (
            &self.capsules,
            &self.content,
            &self.refs,
            &self.indexed,
            &self.counters,
            &self.namespaces,
            &self.names,
        )
            .transaction(
                |(capsules, content, refs, indexed, counters, namespaces, names)| {
                    let trees = IndexTrees {
                        capsules,
                        content,
                        refs,
                        indexed,
                        namespaces,
                        names,
                    };
                    apply_record(record, &trees)?;
                    counters.insert(JOURNAL_POSITION_KEY, position.clone())?;
                    Ok(())
                },
            );
        match result {
            Ok(()) => Ok(()),
            Err(TransactionError::Abort(err)) => Err(err),
            Err(TransactionError::Storage(err)) => Err(err.into()),
        }
    }

    fn set_journal_position(&self, position: &JournalPosition) -> Result<()> {
        self.counters
            .insert(JOURNAL_POSITION_KEY, bincode::serialize(position)?)?;
        Ok(())
    }

    fn journal_position(&self) -> Result<Option<JournalPosition>> {
        Ok(self
            .counters
            .get(JOURNAL_POSITION_KEY)?
            .map(|v| bincode::deserialize(&v))
            .transpose()?)
    }

    fn clear_index(&self) -> Result<()> {
        self.capsules.clear()?;
        self.content.clear()?;
        self.refs.clear()?;
        self.indexed.clear()?;
        self.namespaces.clear()?;
        self.names.clear()?;
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
            out.insert(bincode::deserialize(&key)?, bincode::deserialize(&value)?);
        }
        Ok(out)
    }

    fn scrub_times(&self) -> Result<Vec<(SegmentId, ScrubTimes)>> {
        let mut out = Vec::new();
        for entry in self.scrub_schedule.iter() {
            let (key, value) = entry?;
            let (light, deep): (u64, u64) = bincode::deserialize(&value)?;
            out.push((
                bincode::deserialize(&key)?,
                ScrubTimes {
                    light: scrub_time(light),
                    deep: scrub_time(deep),
                },
            ));
        }
        Ok(out)
    }

    fn put_scrub_times(&self, changes: &[(SegmentId, Option<ScrubTimes>)]) -> Result<()> {
        let mut batch = sled::Batch::default();
        for (segment, times) in changes {
            let key = bincode::serialize(segment)?;
            match times {
                Some(t) => batch.insert(
                    key,
                    bincode::serialize(&(scrub_millis(t.light), scrub_millis(t.deep)))?,
                ),
                None => batch.remove(key),
            }
        }
        self.scrub_schedule.apply_batch(batch)?;
        self.flush()
    }

    fn deregister_unreferenced(&self, hash: &ContentHash, segment: SegmentId) -> Result<bool> {
        let seg_key = bincode::serialize(&segment)?;
        if self.refs.contains_key(&seg_key)? {
            return Ok(false);
        }
        let key = bincode::serialize(hash)?;
        let removed = self
            .content
            .compare_and_swap(&key, Some(seg_key.clone()), None as Option<Vec<u8>>)?
            .is_ok();
        if removed {
            self.indexed.remove(seg_key)?;
        }
        Ok(removed)
    }

    fn get_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>> {
        let key = bincode::serialize(hash)?;
        match self.content.get(key)? {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    fn put_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<()> {
        let key = bincode::serialize(hash)?;
        let val = bincode::serialize(&segment)?;
        self.content.insert(key, val)?;
        self.flush()?;
        Ok(())
    }

    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>> {
        let mut entries = Vec::new();
        for entry in self.content.iter() {
            let (key, value) = entry?;
            let hash: ContentHash = bincode::deserialize(&key)?;
            let segment: SegmentId = bincode::deserialize(&value)?;
            entries.push((hash, segment));
        }
        Ok(entries)
    }

    fn get_namespace(&self, ns: &str) -> Result<Option<u64>> {
        Ok(self
            .namespaces
            .get(ns.as_bytes())?
            .map(|v| bincode::deserialize(&v))
            .transpose()?)
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

    fn create_snapshot(&self) -> Result<Vec<u8>> {
        let header = SnapshotHeader {
            next_segment_id: self.current_segment_counter()?,
            capsule_entries: self.capsules.len() as u64,
            content_entries: self.content.len() as u64,
        };

        let mut out = Vec::new();
        {
            let mut writer = BufWriter::new(&mut out);
            bincode::serialize_into(&mut writer, &header)?;

            for entry in self.capsules.iter() {
                let (_, value) = entry?;
                let capsule: Capsule = decode_capsule(&value)?;
                bincode::serialize_into(&mut writer, &capsule)?;
            }

            for entry in self.content.iter() {
                let (key, value) = entry?;
                let hash: ContentHash = bincode::deserialize(&key)?;
                let segment: SegmentId = bincode::deserialize(&value)?;
                bincode::serialize_into(&mut writer, &(hash, segment))?;
            }

            writer.flush()?;
        }

        Ok(out)
    }

    fn restore_snapshot(&self, data: &[u8]) -> Result<()> {
        let mut reader = Cursor::new(data);
        let header: SnapshotHeader =
            bincode::deserialize_from(&mut reader).context("failed to read snapshot header")?;

        self.capsules.clear()?;
        self.content.clear()?;

        for _ in 0..header.capsule_entries {
            let capsule: Capsule =
                bincode::deserialize_from(&mut reader).context("failed to read capsule entry")?;
            self.put_capsule(&capsule)?;
        }

        for _ in 0..header.content_entries {
            let (hash, segment): (ContentHash, SegmentId) =
                bincode::deserialize_from(&mut reader).context("failed to read content entry")?;
            self.put_content(&hash, segment)?;
        }

        self.counters.insert(
            NEXT_SEGMENT_KEY,
            bincode::serialize(&header.next_segment_id)?,
        )?;

        self.flush()?;
        Ok(())
    }
}

/// The trees a journal record changes, inside one transaction.
struct IndexTrees<'a> {
    capsules: &'a TransactionalTree,
    content: &'a TransactionalTree,
    refs: &'a TransactionalTree,
    indexed: &'a TransactionalTree,
    namespaces: &'a TransactionalTree,
    names: &'a TransactionalTree,
}

fn abort<E: std::fmt::Display>(err: E) -> ConflictableTransactionError<anyhow::Error> {
    ConflictableTransactionError::Abort(anyhow!("{err}"))
}

fn ser<T: Serialize>(value: &T) -> ConflictableTransactionResult<Vec<u8>, anyhow::Error> {
    bincode::serialize(value).map_err(abort)
}

fn read_count(
    refs: &TransactionalTree,
    key: &[u8],
) -> ConflictableTransactionResult<u32, anyhow::Error> {
    match refs.get(key)? {
        Some(v) => bincode::deserialize(&v).map_err(abort),
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
            let key = ser(&manifest.capsule_id)?;
            if t.capsules.get(&key)?.is_some() {
                return Ok(());
            }
            t.capsules.insert(key, ser(&manifest.to_capsule())?)?;
            for entry in &manifest.entries {
                let seg_key = ser(&entry.segment_id)?;
                let count = read_count(t.refs, &seg_key)?;
                t.refs
                    .insert(seg_key.clone(), ser(&count.saturating_add(1))?)?;
                if manifest.dedupe && entry.content_hash != [0u8; 32] {
                    let hash_key = ser(&ContentHash::from_bytes(&entry.content_hash))?;
                    if t.content.get(&hash_key)?.is_none() {
                        t.content.insert(hash_key.clone(), seg_key.clone())?;
                        t.indexed.insert(seg_key, hash_key)?;
                    }
                }
            }
        }
        JournalRecord::Drop(id) => {
            let Some(bytes) = t.capsules.remove(ser(id)?)? else {
                return Ok(());
            };
            let capsule = decode_capsule_bytes(&bytes).map_err(abort)?;
            for segment in &capsule.segments {
                let seg_key = ser(segment)?;
                let count = read_count(t.refs, &seg_key)?;
                if count > 1 {
                    t.refs.insert(seg_key, ser(&(count - 1))?)?;
                    continue;
                }
                t.refs.remove(seg_key.clone())?;
                if let Some(hash_key) = t.indexed.remove(seg_key.clone())? {
                    if t.content.get(&hash_key)?.as_deref() == Some(seg_key.as_slice()) {
                        t.content.remove(hash_key)?;
                    }
                }
            }
        }
        JournalRecord::Namespace { ns, created_at } => {
            // A namespace keeps the time it was first created.
            if t.namespaces.get(ns.as_bytes())?.is_none() {
                t.namespaces.insert(ns.as_bytes(), ser(created_at)?)?;
            }
        }
        JournalRecord::Bind {
            ns,
            name,
            capsule_id,
            metadata,
        } => {
            t.names
                .insert(name_key(ns, name), ser(&(capsule_id, metadata))?)?;
        }
        JournalRecord::Unbind { ns, name } => {
            t.names.remove(name_key(ns, name))?;
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
                        }),
                    ),
                    (
                        SegmentId(8),
                        Some(ScrubTimes {
                            light: Some(light),
                            deep: None,
                        }),
                    ),
                    (
                        SegmentId(9),
                        Some(ScrubTimes {
                            light: Some(light),
                            deep: None,
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
                        deep: Some(deep)
                    }
                ),
                (
                    SegmentId(8),
                    ScrubTimes {
                        light: Some(light),
                        deep: None
                    }
                ),
            ]
        );
        drop(store);
        let _ = std::fs::remove_dir_all(&dir);
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
    // (#113).
    const CAPSULE_ID_HEX: &str = "100000000000000000112233445566778899aabbccddeeff";
    const CAPSULE_HEX: &str = concat!(
        "100000000000000000112233445566778899aabbccddeeff0010000000000000",
        "02000000000000000100000000000000020000000000000000f1536500000000",
        "01000000010000000101100e0000000000000000000000000000000000000000",
        "00400000000000060000000000000002000000000000003333333f0000000000",
        "000000000000000000000000000000000700000000000000",
        "01abababababababababababababababababababababababababababababababab",
    );
    const SEGMENT_ID_HEX: &str = "2a00000000000000";

    #[test]
    fn capsule_records_keep_their_on_disk_encoding() {
        let capsule = sample_capsule();
        assert_eq!(
            to_hex(&bincode::serialize(&capsule.id).unwrap()),
            CAPSULE_ID_HEX
        );
        assert_eq!(to_hex(&bincode::serialize(&capsule).unwrap()), CAPSULE_HEX);

        let bytes: Vec<u8> = (0..CAPSULE_HEX.len() / 2)
            .map(|i| u8::from_str_radix(&CAPSULE_HEX[2 * i..2 * i + 2], 16).unwrap())
            .collect();
        let decoded = decode_capsule_bytes(&bytes).unwrap();
        assert_eq!(decoded.id, capsule.id);
        assert_eq!(decoded.size, capsule.size);
        assert_eq!(decoded.segments, capsule.segments);
        assert_eq!(decoded.created_at, capsule.created_at);
        assert_eq!(decoded.deduped_bytes, capsule.deduped_bytes);
    }

    #[test]
    fn name_keys_keep_their_on_disk_encoding() {
        assert_eq!(name_key("s3:demo", "dir/a.txt"), b"s3:demo\0dir/a.txt");
        assert_eq!(name_key("s3:demo", ""), b"s3:demo\0");

        let id = sample_capsule().id;
        let metadata = vec![("a".to_string(), "bc".to_string())];
        let value = bincode::serialize(&(id, &metadata)).unwrap();
        assert_eq!(
            to_hex(&value),
            format!(
                "{CAPSULE_ID_HEX}{}{}{}{}{}",
                "0100000000000000", "0100000000000000", "61", "0200000000000000", "6263"
            )
        );
        let binding = decode_binding(&value).unwrap();
        assert_eq!((binding.capsule_id, binding.metadata), (id, metadata));
    }

    #[test]
    fn content_and_counter_records_keep_their_on_disk_encoding() {
        let hash = ContentHash("ab".repeat(32));
        let expected_hash = format!("4000000000000000{}", "6162".repeat(32));
        assert_eq!(to_hex(&bincode::serialize(&hash).unwrap()), expected_hash);
        assert_eq!(
            to_hex(&bincode::serialize(&SegmentId(42)).unwrap()),
            SEGMENT_ID_HEX
        );
        assert_eq!(
            to_hex(&bincode::serialize(&42_u64).unwrap()),
            SEGMENT_ID_HEX
        );
    }
}
