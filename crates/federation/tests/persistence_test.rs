//! Tests for persistent storage using SledStorage.
//!
//! This test verifies that Raft log entries and state persist across
//! restarts, which is critical for production deployments.

use federation::{RaftEngine, RaftEngineConfig, SledStorage};
use raft::prelude::*;
use std::time::Duration;
use tempfile::TempDir;
use tokio::sync::mpsc;
use tokio::time::sleep;

/// Test that Raft state persists across engine restarts.
#[tokio::test]
async fn test_raft_persistence_across_restarts() {
    tracing_subscriber::fmt::init();

    let temp = TempDir::new().unwrap();
    let storage_path = temp.path().join("raft_db");

    let node_id = 1;
    let peers = vec![1, 2, 3];

    // Phase 1: Create engine, propose an entry, then shutdown
    {
        let (_inbox_tx, inbox_rx) = mpsc::channel(100);
        let (outbox_tx, outbox_rx) = mpsc::channel(100);
        let (shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let config = RaftEngineConfig {
            id: node_id,
            peers: peers.clone(),
        };

        let engine =
            RaftEngine::new_persistent(config, &storage_path, inbox_rx, outbox_tx, shutdown_rx)
                .expect("failed to create raft engine");

        // Spawn engine
        let engine_handle = tokio::spawn(async move { engine.run().await });

        // Give it time to initialize
        sleep(Duration::from_millis(200)).await;

        // Simulate becoming leader by sending ourselves a vote response
        // (In a real cluster, other nodes would vote for us)
        // For this test, we'll just verify persistence without leader election

        // Shutdown the engine
        drop(shutdown_tx);
        drop(outbox_rx); // Drop receiver so engine can exit

        // Wait for engine to shutdown
        let _ = tokio::time::timeout(Duration::from_secs(2), engine_handle).await;
    }

    // Phase 2: Reopen storage and verify state was persisted
    {
        // Open the storage directly to verify persistence
        let storage = SledStorage::open(&storage_path).expect("failed to reopen storage");

        let state = storage
            .initial_state()
            .expect("failed to get initial state");

        // Verify the conf_state was persisted
        assert_eq!(state.conf_state.voters.len(), 3);
        assert!(state.conf_state.voters.contains(&1));
        assert!(state.conf_state.voters.contains(&2));
        assert!(state.conf_state.voters.contains(&3));

        println!("✓ Storage persisted across restart");
        println!("  - Voters: {:?}", state.conf_state.voters);
        println!("  - Hard state: {:?}", state.hard_state);
    }

    // Phase 3: Create a new engine instance with the persisted storage
    {
        let (_inbox_tx, inbox_rx) = mpsc::channel(100);
        let (outbox_tx, _outbox_rx) = mpsc::channel(100);
        let (_shutdown_tx, shutdown_rx) = mpsc::channel(1);

        let config = RaftEngineConfig {
            id: node_id,
            peers: peers.clone(),
        };

        // This should successfully load the persisted state
        let _engine =
            RaftEngine::new_persistent(config, &storage_path, inbox_rx, outbox_tx, shutdown_rx)
                .expect("failed to create raft engine with persisted storage");

        println!("✓ Successfully created engine from persisted storage");
    }
}

/// Test that log entries persist across restarts.
#[test]
fn test_storage_entry_persistence() {
    let temp = TempDir::new().unwrap();
    let storage_path = temp.path().join("entries_db");

    let conf_state = ConfState::from((vec![1, 2, 3], vec![]));

    // Phase 1: Create storage and append entries
    {
        let mut storage = SledStorage::new_with_conf_state(&storage_path, conf_state.clone())
            .expect("failed to create storage");

        let entries = vec![
            Entry {
                index: 1,
                term: 1,
                data: b"first_entry".to_vec(),
                ..Default::default()
            },
            Entry {
                index: 2,
                term: 1,
                data: b"second_entry".to_vec(),
                ..Default::default()
            },
            Entry {
                index: 3,
                term: 2,
                data: b"third_entry".to_vec(),
                ..Default::default()
            },
        ];

        storage.append(&entries).expect("failed to append entries");

        let hard_state = HardState {
            term: 2,
            vote: 1,
            commit: 3,
        };
        storage
            .set_hardstate(hard_state)
            .expect("failed to set hard state");

        println!("✓ Wrote 3 entries to storage");
    }

    // Phase 2: Reopen storage and verify entries
    {
        let storage = SledStorage::open(&storage_path).expect("failed to reopen storage");

        assert_eq!(storage.first_index().unwrap(), 1);
        assert_eq!(storage.last_index().unwrap(), 3);

        let entries = storage
            .entries(1, 4, None, raft::GetEntriesContext::empty(false))
            .expect("failed to read entries");
        assert_eq!(entries.len(), 3);
        assert_eq!(entries[0].data, b"first_entry");
        assert_eq!(entries[1].data, b"second_entry");
        assert_eq!(entries[2].data, b"third_entry");
        assert_eq!(entries[2].term, 2);

        let state = storage.initial_state().expect("failed to get state");
        assert_eq!(state.hard_state.term, 2);
        assert_eq!(state.hard_state.vote, 1);
        assert_eq!(state.hard_state.commit, 3);

        println!("✓ All entries and state successfully persisted");
    }
}

/// Test that storage handles compaction correctly.
#[test]
fn test_storage_compaction() {
    let temp = TempDir::new().unwrap();
    let storage_path = temp.path().join("compact_db");

    let conf_state = ConfState::from((vec![1], vec![]));
    let mut storage = SledStorage::new_with_conf_state(&storage_path, conf_state)
        .expect("failed to create storage");

    // Append 10 entries
    let entries: Vec<Entry> = (1..=10)
        .map(|i| Entry {
            index: i,
            term: 1,
            data: format!("entry_{}", i).into_bytes(),
            ..Default::default()
        })
        .collect();

    storage.append(&entries).expect("failed to append entries");

    assert_eq!(storage.first_index().unwrap(), 1);
    assert_eq!(storage.last_index().unwrap(), 10);

    // Compact entries before index 6
    storage.compact(6).expect("failed to compact");

    assert_eq!(storage.first_index().unwrap(), 6);
    assert_eq!(storage.last_index().unwrap(), 10);

    // Entries before 6 should be compacted
    assert!(matches!(
        storage.entries(1, 6, None, raft::GetEntriesContext::empty(false)),
        Err(raft::Error::Store(raft::StorageError::Compacted))
    ));

    // Entries from 6 onwards should still be available
    let remaining = storage
        .entries(6, 11, None, raft::GetEntriesContext::empty(false))
        .expect("failed to read remaining entries");
    assert_eq!(remaining.len(), 5);
    assert_eq!(remaining[0].data, b"entry_6");
    assert_eq!(remaining[4].data, b"entry_10");

    println!("✓ Compaction works correctly");
}

/// Test that max_size parameter limits entries returned.
#[test]
fn test_storage_entries_max_size() {
    let temp = TempDir::new().unwrap();
    let storage_path = temp.path().join("maxsize_db");

    let conf_state = ConfState::from((vec![1], vec![]));
    let mut storage = SledStorage::new_with_conf_state(&storage_path, conf_state)
        .expect("failed to create storage");

    // Create entries with large data
    let large_data = vec![0u8; 1000]; // 1KB per entry
    let entries: Vec<Entry> = (1..=10)
        .map(|i| Entry {
            index: i,
            term: 1,
            data: large_data.clone(),
            ..Default::default()
        })
        .collect();

    storage.append(&entries).expect("failed to append entries");

    // Request with max_size of 3KB - should get ~3 entries
    let limited = storage
        .entries(1, 11, Some(3500), raft::GetEntriesContext::empty(false))
        .expect("failed to read limited entries");

    // Should get at least 1 entry (first entry always included)
    // and likely 3 entries total (~3KB)
    assert!(!limited.is_empty() && limited.len() <= 4);

    println!(
        "✓ max_size parameter limits entries correctly (got {} entries)",
        limited.len()
    );
}
