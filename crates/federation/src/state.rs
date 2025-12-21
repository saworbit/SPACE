use anyhow::{Context, Result};
use common::CapsuleId;
use std::path::Path;

#[derive(Clone)]
pub struct ReplicationState {
    tree: sled::Tree,
}

impl ReplicationState {
    pub fn open(path: &Path) -> Result<Self> {
        let db = sled::open(path)
            .with_context(|| format!("open federation state {}", path.display()))?;
        Ok(Self {
            tree: db.open_tree("state")?,
        })
    }

    pub fn is_synced(&self, capsule_id: CapsuleId, target_zone: &str) -> Result<bool> {
        let key = format!("{}/{}", capsule_id.as_uuid(), target_zone);
        Ok(self.tree.contains_key(key.as_bytes())?)
    }

    pub fn mark_synced(&self, capsule_id: CapsuleId, target_zone: &str) -> Result<()> {
        let key = format!("{}/{}", capsule_id.as_uuid(), target_zone);
        self.tree.insert(key.as_bytes(), b"1")?;
        self.tree.flush()?;
        Ok(())
    }
}
