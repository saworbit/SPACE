use anyhow::{anyhow, Context, Result};
use common::{Capsule, CapsuleId, ContentHash, SegmentId};
use serde::{Deserialize, Serialize};
use sled::Db;

const NEXT_SEGMENT_KEY: &[u8] = b"next_segment_id";

/// Abstraction over the metadata storage backend.
/// Allows swapping Sled for other engines (e.g., RocksDB, in-memory) without
/// changing the registry surface.
pub trait MetadataStore: Send + Sync {
    fn get_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>>;
    fn put_capsule(&self, capsule: &Capsule) -> Result<()>;
    fn delete_capsule(&self, id: &CapsuleId) -> Result<Option<Capsule>>;
    fn list_capsules(&self) -> Result<Vec<Capsule>>;
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
struct Snapshot {
    next_segment_id: u64,
    capsules: Vec<Capsule>,
    content: Vec<(ContentHash, SegmentId)>,
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
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
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
            Some(bytes) => Ok(Some(bincode::deserialize(&bytes)?)),
            None => Ok(None),
        }
    }

    fn list_capsules(&self) -> Result<Vec<Capsule>> {
        let mut capsules = Vec::new();
        for entry in self.capsules.iter() {
            let (_, value) = entry?;
            capsules.push(bincode::deserialize(&value)?);
        }
        Ok(capsules)
    }

    fn add_deduped_bytes(&self, id: &CapsuleId, bytes: u64) -> Result<()> {
        let key = bincode::serialize(id)?;
        let updated = self
            .capsules
            .fetch_and_update(key, |existing| -> Option<Vec<u8>> {
                let data = existing?;
                let mut capsule: Capsule = bincode::deserialize(data).ok()?;
                capsule.deduped_bytes = capsule.deduped_bytes.saturating_add(bytes);
                bincode::serialize(&capsule).ok()
            })?;

        if updated.is_none() {
            return Err(anyhow!("capsule {:?} not found for dedup update", id));
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
        let snapshot = Snapshot {
            next_segment_id: self.current_segment_counter()?,
            capsules: self.list_capsules()?,
            content: self.list_content()?,
        };
        Ok(bincode::serialize(&snapshot)?)
    }

    fn restore_snapshot(&self, data: &[u8]) -> Result<()> {
        let snapshot: Snapshot = bincode::deserialize(data)
            .with_context(|| "failed to deserialize metadata snapshot")?;

        self.capsules.clear()?;
        self.content.clear()?;

        for capsule in snapshot.capsules {
            self.put_capsule(&capsule)?;
        }

        for (hash, segment) in snapshot.content {
            self.put_content(&hash, segment)?;
        }

        self.counters.insert(
            NEXT_SEGMENT_KEY,
            bincode::serialize(&snapshot.next_segment_id)?,
        )?;

        self.flush()?;
        Ok(())
    }
}
