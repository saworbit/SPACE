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

    /// Atomically checks if synced, and marks as synced if not.
    ///
    /// This prevents the TOCTOU race condition where two threads both see
    /// `is_synced() == false` and proceed to duplicate replication work.
    ///
    /// Returns `true` if it was ALREADY synced (caller should skip work),
    /// `false` if it was not synced and is now marked (caller should proceed).
    pub fn try_mark_synced(&self, capsule_id: CapsuleId, target_zone: &str) -> Result<bool> {
        let key = format!("{}/{}", capsule_id.as_uuid(), target_zone);
        let key_bytes = key.as_bytes();

        // compare_and_swap(key, expected_old_value, new_value)
        // We expect it to be None (not present). If it is None, we insert "1".
        // If it returns Ok(Ok(())), the swap happened (it was not synced, now marked).
        // If it returns Ok(Err(_)), the value existed (already synced).
        let result = self
            .tree
            .compare_and_swap(key_bytes, None as Option<&[u8]>, Some(b"1"))?;

        if result.is_ok() {
            // Successfully marked - flush to ensure durability
            self.tree.flush()?;
        }

        // is_err() means CAS failed because value already existed = already synced
        Ok(result.is_err())
    }

    /// Removes the synced marker, allowing the capsule to be re-replicated.
    ///
    /// Used for error recovery when replication fails after claiming.
    pub fn unmark_synced(&self, capsule_id: CapsuleId, target_zone: &str) -> Result<()> {
        let key = format!("{}/{}", capsule_id.as_uuid(), target_zone);
        self.tree.remove(key.as_bytes())?;
        self.tree.flush()?;
        Ok(())
    }
}
