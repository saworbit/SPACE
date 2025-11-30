use anyhow::{anyhow, Context, Result};
use common::{Capsule, CapsuleId, ContentHash, SegmentId};
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
    fn list_capsules(&self) -> Result<Vec<Capsule>>;
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
                let capsule: Capsule = bincode::deserialize(&value)?;
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
