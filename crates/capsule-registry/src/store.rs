use anyhow::{anyhow, Context, Result};
use common::{
    Capsule, CapsuleId, CompressionPolicy, ContentHash, CryptoProfile, EncryptionPolicy,
    LayoutPolicy, Policy, SegmentId,
};
use serde::{Deserialize, Serialize};
use sled::Db;
use std::io::{BufWriter, Cursor, Write};

const NEXT_SEGMENT_KEY: &[u8] = b"next_segment_id";

/// Abstraction over the metadata storage backend.
/// Allows swapping Sled for other engines (e.g., RocksDB, in-memory) without
/// changing the registry surface.
pub trait MetadataStore: Send + Sync {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>>;
    fn put_capsule(&self, capsule: &Capsule) -> Result<()>;
    fn delete_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>>;
    fn list_capsules(&self, limit: usize, start_after: Option<CapsuleId>) -> Result<Vec<Capsule>>;
    #[allow(dead_code)]
    fn add_deduped_bytes(&self, id: &CapsuleId, bytes: u64) -> Result<()>;

    fn allocate_segment_id(&self) -> Result<SegmentId>;

    fn get_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>>;
    fn put_content(&self, hash: &ContentHash, segment: SegmentId) -> Result<()>;
    fn delete_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>>;
    fn list_content(&self) -> Result<Vec<(ContentHash, SegmentId)>>;

    #[allow(dead_code)]
    fn create_snapshot(&self) -> Result<Vec<u8>>;
    #[allow(dead_code)]
    fn restore_snapshot(&self, data: &[u8]) -> Result<()>;
}

/// Snapshot payload persisted for Raft state machine checkpoints.
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
    #[cfg(feature = "podms")]
    #[serde(default = "default_duration_60s")]
    pub rpo: std::time::Duration,
    #[cfg(feature = "podms")]
    #[serde(default = "default_duration_10ms")]
    pub latency_target: std::time::Duration,
    #[cfg(feature = "podms")]
    #[serde(default)]
    pub sovereignty: common::podms::SovereigntyLevel,
    #[cfg(feature = "podms")]
    #[serde(default = "default_replica_count_3")]
    pub replica_count: u8,
}

#[cfg(feature = "podms")]
fn default_duration_60s() -> std::time::Duration {
    std::time::Duration::from_secs(60)
}

#[cfg(feature = "podms")]
fn default_duration_10ms() -> std::time::Duration {
    std::time::Duration::from_millis(10)
}

#[cfg(feature = "podms")]
fn default_replica_count_3() -> u8 {
    3
}

impl From<PolicyLegacy> for Policy {
    fn from(value: PolicyLegacy) -> Self {
        let mut policy = Policy {
            compression: value.compression,
            dedupe: value.dedupe,
            compact_interval_secs: value.compact_interval_secs,
            erasure_profile: value.erasure_profile,
            encryption: value.encryption,
            crypto_profile: value.crypto_profile,
            layout: value.layout,
            federation: None,
            ..Policy::default()
        };

        #[cfg(feature = "podms")]
        {
            policy.rpo = value.rpo;
            policy.latency_target = value.latency_target;
            policy.sovereignty = value.sovereignty;
            policy.replica_count = value.replica_count;
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
        Err(err) if is_unexpected_eof(&err) => bincode::deserialize::<CapsuleLegacy>(bytes)
            .map(Into::into)
            .or(Err(err)),
        Err(err) => Err(err),
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
    counters: sled::Tree,
}

impl SledStore {
    pub fn open(path: &str) -> Result<Self> {
        let db = sled::open(path)?;
        let capsules = db.open_tree("capsules")?;
        let content = db.open_tree("content")?;
        let counters = db.open_tree("counters")?;

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
            counters,
        })
    }

    fn flush(&self) -> Result<()> {
        self.db.flush()?;
        Ok(())
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

    fn delete_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>> {
        let key = bincode::serialize(id)?;
        let removed = self.capsules.remove(key)?;
        self.flush()?;
        match removed {
            Some(bytes) => Ok(Some(decode_capsule(&bytes)?)),
            None => Ok(None),
        }
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

    fn delete_content(&self, hash: &ContentHash) -> Result<Option<SegmentId>> {
        let key = bincode::serialize(hash)?;
        let removed = self.content.remove(key)?;
        self.flush()?;
        match removed {
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
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
