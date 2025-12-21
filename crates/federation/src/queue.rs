use anyhow::{Context, Result};
use common::{CapsuleId, TransferPriority};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ReplicationJob {
    pub capsule_id: CapsuleId,
    pub target_zone: String,
    pub priority: TransferPriority,
}

#[derive(Clone)]
pub struct ReplicationQueue {
    critical: sled::Tree,
    background: sled::Tree,
}

impl ReplicationQueue {
    pub fn open(path: &Path) -> Result<Self> {
        let db = sled::open(path)
            .with_context(|| format!("open federation queue {}", path.display()))?;
        Ok(Self {
            critical: db.open_tree("critical")?,
            background: db.open_tree("background")?,
        })
    }

    pub fn enqueue(&self, job: &ReplicationJob) -> Result<bool> {
        let tree = match job.priority {
            TransferPriority::Critical => &self.critical,
            TransferPriority::Background => &self.background,
        };

        let key = format!("{}/{}", job.capsule_id.as_uuid(), job.target_zone);
        let inserted = tree.insert(key.as_bytes(), bincode::serialize(job)?)?;
        tree.flush()?;
        Ok(inserted.is_none())
    }

    pub fn dequeue_next(&self) -> Result<Option<ReplicationJob>> {
        if let Some(job) = self.dequeue_from_tree(&self.critical)? {
            return Ok(Some(job));
        }
        self.dequeue_from_tree(&self.background)
    }

    fn dequeue_from_tree(&self, tree: &sled::Tree) -> Result<Option<ReplicationJob>> {
        let mut iter = tree.iter();
        let Some(item) = iter.next() else {
            return Ok(None);
        };
        let (key, value) = item?;
        let job: ReplicationJob = bincode::deserialize(&value)?;
        tree.remove(key)?;
        tree.flush()?;
        Ok(Some(job))
    }
}
