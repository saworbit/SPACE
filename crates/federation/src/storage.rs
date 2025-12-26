//! Persistent storage for Raft consensus using Sled embedded database.
//!
//! This module provides a disk-backed implementation of the `raft::storage::Storage` trait,
//! replacing the in-memory `MemStorage` used in Phase 9.1.

use anyhow::{Context, Result};
use prost::Message as ProstMessage;
use raft::prelude::*;
use raft::storage::Storage;
use raft::StorageError;
use sled::{Db, Tree};
use std::path::Path;
use std::sync::{Arc, RwLock};

/// Sled-backed persistent storage for Raft.
///
/// This storage implementation uses separate sled trees for different types of data:
/// - `hard_state_tree`: Stores `HardState` (term, vote, commit index)
/// - `conf_state_tree`: Stores `ConfState` (voters, learners)
/// - `entries_tree`: Stores log `Entry` objects indexed by entry index
/// - `snapshot_tree`: Stores snapshot metadata
///
/// A cache layer is used for frequently accessed data to improve performance.
pub struct SledStorage {
    #[allow(dead_code)] // Needed to keep the database open
    db: Db,
    hard_state_tree: Tree,
    conf_state_tree: Tree,
    entries_tree: Tree,
    snapshot_tree: Tree,
    /// Cache for fast reads of frequently accessed data
    cache: Arc<RwLock<StorageCache>>,
}

/// Cached state for fast reads
struct StorageCache {
    first_index: u64,
    last_index: u64,
    hard_state: HardState,
    conf_state: ConfState,
    snapshot: Snapshot,
}

impl SledStorage {
    /// Open an existing Sled database at the given path.
    ///
    /// # Errors
    /// Returns an error if:
    /// - The database cannot be opened
    /// - The database is corrupted or invalid
    pub fn open<P: AsRef<Path>>(path: P) -> Result<Self> {
        let db = sled::open(path).context("failed to open sled database")?;

        let hard_state_tree = db.open_tree("hard_state")?;
        let conf_state_tree = db.open_tree("conf_state")?;
        let entries_tree = db.open_tree("entries")?;
        let snapshot_tree = db.open_tree("snapshot")?;

        // Load initial state from disk
        let hard_state = load_hard_state(&hard_state_tree)?;
        let conf_state = load_conf_state(&conf_state_tree)?;
        let snapshot = load_snapshot(&snapshot_tree)?;

        let (first_index, last_index) = compute_indices(&entries_tree, &snapshot)?;

        // Validate consistency
        if first_index > last_index + 1 {
            anyhow::bail!(
                "corrupted storage: first_index ({}) > last_index ({}) + 1",
                first_index,
                last_index
            );
        }

        Ok(Self {
            db,
            hard_state_tree,
            conf_state_tree,
            entries_tree,
            snapshot_tree,
            cache: Arc::new(RwLock::new(StorageCache {
                first_index,
                last_index,
                hard_state,
                conf_state,
                snapshot,
            })),
        })
    }

    /// Create a new Sled database with the given initial configuration state.
    ///
    /// This matches the `MemStorage::new_with_conf_state` API for compatibility.
    ///
    /// # Errors
    /// Returns an error if the database cannot be created.
    pub fn new_with_conf_state<P: AsRef<Path>>(path: P, conf_state: ConfState) -> Result<Self> {
        let db = sled::open(path).context("failed to create sled database")?;

        let hard_state_tree = db.open_tree("hard_state")?;
        let conf_state_tree = db.open_tree("conf_state")?;
        let entries_tree = db.open_tree("entries")?;
        let snapshot_tree = db.open_tree("snapshot")?;

        // Initialize with default values
        let hard_state = HardState::default();
        let snapshot = Snapshot::default();

        // Persist initial conf_state
        let mut conf_state_bytes = Vec::new();
        conf_state.encode(&mut conf_state_bytes)?;
        conf_state_tree.insert(b"cs", conf_state_bytes)?;
        conf_state_tree.flush()?;

        Ok(Self {
            db,
            hard_state_tree,
            conf_state_tree,
            entries_tree,
            snapshot_tree,
            cache: Arc::new(RwLock::new(StorageCache {
                first_index: 1,
                last_index: 0,
                hard_state,
                conf_state,
                snapshot,
            })),
        })
    }

    /// Append log entries to the storage.
    ///
    /// This method is called by `RawNode` via `mut_store()`.
    ///
    /// # Errors
    /// Returns an error if the entries cannot be persisted to disk.
    pub fn append(&mut self, entries: &[Entry]) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        for entry in entries {
            // Use Big Endian encoding for correct lexicographic sorting
            let key = entry.index.to_be_bytes();
            let mut value = Vec::new();
            entry.encode(&mut value).context("failed to encode entry")?;
            self.entries_tree.insert(key, value)?;
        }

        // Update cache
        let mut cache = self.cache.write().unwrap();
        if let Some(last) = entries.last() {
            cache.last_index = last.index;
        }
        if let Some(first) = entries.first() {
            if cache.first_index == 0 || first.index < cache.first_index {
                cache.first_index = first.index;
            }
        }

        // Ensure durability
        self.entries_tree.flush()?;
        Ok(())
    }

    /// Set the hard state (term, vote, commit).
    ///
    /// This method is called by `RawNode` via `mut_store()`.
    pub fn set_hardstate(&mut self, hs: HardState) -> Result<()> {
        let mut value = Vec::new();
        hs.encode(&mut value)
            .context("failed to encode hard state")?;
        self.hard_state_tree.insert(b"hs", value)?;
        self.hard_state_tree.flush()?;

        let mut cache = self.cache.write().unwrap();
        cache.hard_state = hs;
        Ok(())
    }

    /// Apply a snapshot to the storage.
    ///
    /// This clears all log entries and updates the snapshot metadata.
    pub fn apply_snapshot(&mut self, snapshot: Snapshot) -> Result<()> {
        // Clear all existing entries
        self.entries_tree.clear()?;

        // Save snapshot
        let mut snapshot_bytes = Vec::new();
        snapshot.encode(&mut snapshot_bytes)?;
        self.snapshot_tree.insert(b"snap", snapshot_bytes)?;
        self.snapshot_tree.flush()?;

        // Update cache
        let mut cache = self.cache.write().unwrap();
        cache.snapshot = snapshot.clone();
        cache.first_index = snapshot.get_metadata().index + 1;
        cache.last_index = snapshot.get_metadata().index;

        // Update conf_state from snapshot
        cache.conf_state = snapshot.get_metadata().get_conf_state().clone();
        let mut conf_state_bytes = Vec::new();
        cache.conf_state.encode(&mut conf_state_bytes)?;
        self.conf_state_tree.insert(b"cs", conf_state_bytes)?;
        self.conf_state_tree.flush()?;

        Ok(())
    }

    /// Set the configuration state.
    ///
    /// This is used when the configuration changes (e.g., adding/removing nodes).
    pub fn set_conf_state(&mut self, conf_state: ConfState) -> Result<()> {
        let mut value = Vec::new();
        conf_state.encode(&mut value)?;
        self.conf_state_tree.insert(b"cs", value)?;
        self.conf_state_tree.flush()?;

        let mut cache = self.cache.write().unwrap();
        cache.conf_state = conf_state;
        Ok(())
    }

    /// Compact the log by removing entries before the given index.
    ///
    /// This is called after applying a snapshot to free up disk space.
    pub fn compact(&mut self, compact_index: u64) -> Result<()> {
        // Remove all entries with index < compact_index
        let keys_to_remove: Vec<_> = self
            .entries_tree
            .range(..compact_index.to_be_bytes())
            .filter_map(|r| r.ok())
            .map(|(k, _)| k)
            .collect();

        for key in keys_to_remove {
            self.entries_tree.remove(key)?;
        }

        self.entries_tree.flush()?;

        // Update cache
        let mut cache = self.cache.write().unwrap();
        if compact_index > cache.first_index {
            cache.first_index = compact_index;
        }

        Ok(())
    }
}

// Implement the raft::storage::Storage trait
impl Storage for SledStorage {
    fn initial_state(&self) -> raft::Result<RaftState> {
        let cache = self.cache.read().unwrap();
        Ok(RaftState {
            hard_state: cache.hard_state.clone(),
            conf_state: cache.conf_state.clone(),
        })
    }

    fn entries(
        &self,
        low: u64,
        high: u64,
        max_size: impl Into<Option<u64>>,
        _context: raft::GetEntriesContext,
    ) -> raft::Result<Vec<Entry>> {
        let max_size = max_size.into();

        if low > high {
            return Err(raft::Error::Store(StorageError::Unavailable));
        }

        let cache = self.cache.read().unwrap();

        // Check bounds
        if low < cache.first_index {
            return Err(raft::Error::Store(StorageError::Compacted));
        }

        if high > cache.last_index + 1 {
            return Err(raft::Error::Store(StorageError::Unavailable));
        }

        drop(cache); // Release lock before disk I/O

        let mut entries = Vec::new();
        let mut total_size: u64 = 0;

        for idx in low..high {
            let key = idx.to_be_bytes();

            match self.entries_tree.get(key) {
                Ok(Some(bytes)) => {
                    let entry: Entry = Entry::decode(&bytes[..])
                        .map_err(|_| raft::Error::Store(StorageError::Unavailable))?;

                    let entry_size = bytes.len() as u64;

                    if let Some(max) = max_size {
                        if total_size > 0 && total_size + entry_size > max {
                            break;
                        }
                        total_size += entry_size;
                    }

                    entries.push(entry);
                }
                Ok(None) => {
                    // Entry is missing (compacted or never existed)
                    return Err(raft::Error::Store(StorageError::Compacted));
                }
                Err(_) => {
                    return Err(raft::Error::Store(StorageError::Unavailable));
                }
            }
        }

        Ok(entries)
    }

    fn term(&self, idx: u64) -> raft::Result<u64> {
        let cache = self.cache.read().unwrap();

        // Check if it's in the snapshot
        if idx == cache.snapshot.get_metadata().index {
            return Ok(cache.snapshot.get_metadata().term);
        }

        if idx < cache.first_index {
            return Err(raft::Error::Store(StorageError::Compacted));
        }

        if idx > cache.last_index {
            return Err(raft::Error::Store(StorageError::Unavailable));
        }

        drop(cache); // Release lock before disk I/O

        let key = idx.to_be_bytes();
        match self.entries_tree.get(key) {
            Ok(Some(bytes)) => {
                let entry: Entry = Entry::decode(&bytes[..])
                    .map_err(|_| raft::Error::Store(StorageError::Unavailable))?;
                Ok(entry.term)
            }
            Ok(None) => Err(raft::Error::Store(StorageError::Compacted)),
            Err(_) => Err(raft::Error::Store(StorageError::Unavailable)),
        }
    }

    fn first_index(&self) -> raft::Result<u64> {
        let cache = self.cache.read().unwrap();
        Ok(cache.first_index)
    }

    fn last_index(&self) -> raft::Result<u64> {
        let cache = self.cache.read().unwrap();
        Ok(cache.last_index)
    }

    fn snapshot(&self, _request_index: u64, _to: u64) -> raft::Result<Snapshot> {
        let cache = self.cache.read().unwrap();
        Ok(cache.snapshot.clone())
    }
}

// Helper functions

/// Load HardState from the tree
fn load_hard_state(tree: &Tree) -> Result<HardState> {
    match tree.get(b"hs")? {
        Some(bytes) => HardState::decode(&bytes[..]).context("failed to decode hard state"),
        None => Ok(HardState::default()),
    }
}

/// Load ConfState from the tree
fn load_conf_state(tree: &Tree) -> Result<ConfState> {
    match tree.get(b"cs")? {
        Some(bytes) => ConfState::decode(&bytes[..]).context("failed to decode conf state"),
        None => Ok(ConfState::default()),
    }
}

/// Load Snapshot from the tree
fn load_snapshot(tree: &Tree) -> Result<Snapshot> {
    match tree.get(b"snap")? {
        Some(bytes) => Snapshot::decode(&bytes[..]).context("failed to decode snapshot"),
        None => Ok(Snapshot::default()),
    }
}

/// Compute first_index and last_index from the entries tree and snapshot
fn compute_indices(entries_tree: &Tree, snapshot: &Snapshot) -> Result<(u64, u64)> {
    let snap_index = snapshot.get_metadata().index;

    // Find first entry
    let first_index = if let Some(Ok((key, _))) = entries_tree.iter().next() {
        u64::from_be_bytes(
            key.as_ref()
                .try_into()
                .context("invalid entry key length")?,
        )
    } else {
        // No entries, start after snapshot
        snap_index + 1
    };

    // Find last entry
    let last_index = if let Some(Ok((key, _))) = entries_tree.iter().next_back() {
        u64::from_be_bytes(
            key.as_ref()
                .try_into()
                .context("invalid entry key length")?,
        )
    } else {
        // No entries, use snapshot index
        snap_index
    };

    Ok((first_index, last_index))
}

#[cfg(test)]
mod tests {
    use super::*;
    use raft::GetEntriesContext;
    use tempfile::TempDir;

    #[test]
    fn test_new_with_conf_state() {
        let temp = TempDir::new().unwrap();
        let conf_state = ConfState::from((vec![1, 2, 3], vec![]));

        let storage = SledStorage::new_with_conf_state(temp.path(), conf_state.clone()).unwrap();

        let state = storage.initial_state().unwrap();
        assert_eq!(state.conf_state, conf_state);
        assert_eq!(storage.first_index().unwrap(), 1);
        assert_eq!(storage.last_index().unwrap(), 0);
    }

    #[test]
    fn test_append_and_retrieve_entries() {
        let temp = TempDir::new().unwrap();
        let conf_state = ConfState::from((vec![1], vec![]));

        let mut storage = SledStorage::new_with_conf_state(temp.path(), conf_state).unwrap();

        let entries = vec![
            Entry {
                index: 1,
                term: 1,
                data: b"entry1".to_vec(),
                ..Default::default()
            },
            Entry {
                index: 2,
                term: 1,
                data: b"entry2".to_vec(),
                ..Default::default()
            },
        ];

        storage.append(&entries).unwrap();

        assert_eq!(storage.first_index().unwrap(), 1);
        assert_eq!(storage.last_index().unwrap(), 2);

        let retrieved = storage
            .entries(1, 3, None, GetEntriesContext::empty(false))
            .unwrap();
        assert_eq!(retrieved.len(), 2);
        assert_eq!(retrieved[0].data, b"entry1");
        assert_eq!(retrieved[1].data, b"entry2");
    }

    #[test]
    #[cfg_attr(
        target_os = "macos",
        ignore = "Sled database has file locking issues on macOS"
    )]
    fn test_persistence_across_restarts() {
        let temp = TempDir::new().unwrap();
        let conf_state = ConfState::from((vec![1, 2, 3], vec![]));

        // Create storage and write entries
        {
            let mut storage =
                SledStorage::new_with_conf_state(temp.path(), conf_state.clone()).unwrap();

            let entries = vec![Entry {
                index: 1,
                term: 1,
                data: b"persistent_entry".to_vec(),
                ..Default::default()
            }];
            storage.append(&entries).unwrap();

            let hs = HardState {
                term: 1,
                vote: 1,
                commit: 1,
            };
            storage.set_hardstate(hs).unwrap();
        }

        // Reopen and verify
        {
            let storage = SledStorage::open(temp.path()).unwrap();
            assert_eq!(storage.last_index().unwrap(), 1);

            let entries = storage
                .entries(1, 2, None, GetEntriesContext::empty(false))
                .unwrap();
            assert_eq!(entries.len(), 1);
            assert_eq!(entries[0].data, b"persistent_entry");

            let state = storage.initial_state().unwrap();
            assert_eq!(state.hard_state.term, 1);
            assert_eq!(state.hard_state.vote, 1);
            assert_eq!(state.hard_state.commit, 1);
        }
    }

    #[test]
    fn test_term() {
        let temp = TempDir::new().unwrap();
        let conf_state = ConfState::from((vec![1], vec![]));

        let mut storage = SledStorage::new_with_conf_state(temp.path(), conf_state).unwrap();

        let entries = vec![
            Entry {
                index: 1,
                term: 1,
                ..Default::default()
            },
            Entry {
                index: 2,
                term: 2,
                ..Default::default()
            },
        ];
        storage.append(&entries).unwrap();

        assert_eq!(storage.term(1).unwrap(), 1);
        assert_eq!(storage.term(2).unwrap(), 2);
    }

    #[test]
    fn test_compact() {
        let temp = TempDir::new().unwrap();
        let conf_state = ConfState::from((vec![1], vec![]));

        let mut storage = SledStorage::new_with_conf_state(temp.path(), conf_state).unwrap();

        let entries: Vec<Entry> = (1..=10)
            .map(|i| Entry {
                index: i,
                term: 1,
                ..Default::default()
            })
            .collect();
        storage.append(&entries).unwrap();

        assert_eq!(storage.first_index().unwrap(), 1);
        assert_eq!(storage.last_index().unwrap(), 10);

        // Compact entries before index 5
        storage.compact(5).unwrap();

        assert_eq!(storage.first_index().unwrap(), 5);
        assert_eq!(storage.last_index().unwrap(), 10);

        // Entries before 5 should be compacted
        assert!(matches!(
            storage.entries(1, 5, None, GetEntriesContext::empty(false)),
            Err(raft::Error::Store(StorageError::Compacted))
        ));

        // Entries from 5 onwards should still be available
        let remaining = storage
            .entries(5, 11, None, GetEntriesContext::empty(false))
            .unwrap();
        assert_eq!(remaining.len(), 6);
    }
}
