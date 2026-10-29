use anyhow::{anyhow, Context, Result};
use common::{
    Capsule, CapsuleId, CompressionPolicy, ContentHash, CryptoProfile, EncryptionPolicy,
    FederationStrategy, LayoutPolicy, Policy, SegmentId,
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

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }

    // Byte-for-byte encodings written by the current release. A change here
    // means existing space.db stores can no longer be read without a
    // migration, so update these only together with one (#113).
    const CAPSULE_ID_HEX: &str = "100000000000000000112233445566778899aabbccddeeff";
    const CAPSULE_HEX: &str = concat!(
        "100000000000000000112233445566778899aabbccddeeff0010000000000000",
        "02000000000000000100000000000000020000000000000000f1536500000000",
        "01000000010000000101100e0000000000000000000000000000000000000000",
        "00400000000000060000000000000002000000000000003333333f0000000000",
        "000000000000000000000000000000000700000000000000",
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
