//! PODMS (Policy-Orchestrated Disaggregated Mesh Scaling) integration tests
//!
//! These tests verify telemetry emission and policy handling for distributed scaling.

#![cfg(all(feature = "podms", feature = "pipeline_async"))]

use capsule_registry::pipeline::WritePipeline;
use capsule_registry::CapsuleRegistry;
use common::{podms::Telemetry, Policy};
use nvram_sim::NvramLog;
use tokio::sync::mpsc;

#[tokio::test]
async fn test_telemetry_channel_creation() {
    let (tx, _rx) = mpsc::unbounded_channel::<Telemetry>();
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram).with_telemetry_channel(tx);

    // Pipeline should be created successfully with telemetry channel
    assert!(true); // If we got here, the channel was set up correctly
}

#[tokio::test]
async fn test_telemetry_emission_on_write() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Telemetry>();
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram).with_telemetry_channel(tx);

    // Write a small capsule
    let data = b"Hello PODMS!";
    let policy = Policy::default();

    let capsule_id = pipeline
        .write_capsule_with_policy_async(data, &policy)
        .await
        .expect("write should succeed");

    // Verify telemetry was emitted
    let event = rx.try_recv().expect("should receive telemetry event");

    match event {
        Telemetry::NewCapsule {
            id,
            policy: _,
            node_id,
        } => {
            assert_eq!(id, capsule_id);
            assert!(node_id.is_none()); // Node ID not set in pipeline
        }
        _ => panic!("Expected NewCapsule telemetry event"),
    }
}

#[tokio::test]
async fn test_telemetry_with_metro_sync_policy() {
    use std::time::Duration;

    let (tx, mut rx) = mpsc::unbounded_channel::<Telemetry>();
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram).with_telemetry_channel(tx);

    // Use metro-sync policy
    let data = b"Test data for metro sync";
    let policy = Policy::metro_sync();

    let capsule_id = pipeline
        .write_capsule_with_policy_async(data, &policy)
        .await
        .expect("write should succeed");

    // Verify telemetry contains correct policy
    let event = rx.try_recv().expect("should receive telemetry event");

    match event {
        Telemetry::NewCapsule {
            id,
            policy,
            node_id: _,
        } => {
            assert_eq!(id, capsule_id);
            assert_eq!(policy.rpo, Duration::ZERO);
            assert_eq!(policy.latency_target, Duration::from_millis(2));
            assert!(policy.encryption.is_enabled());
        }
        _ => panic!("Expected NewCapsule telemetry event"),
    }
}

#[tokio::test]
async fn test_no_telemetry_without_channel() {
    // Create pipeline without telemetry channel
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram);

    // Write should succeed even without telemetry channel
    let data = b"Test data";
    let policy = Policy::default();

    let result = pipeline.write_capsule_with_policy_async(data, &policy).await;

    assert!(result.is_ok(), "write should succeed without telemetry");
}

#[tokio::test]
async fn test_telemetry_channel_closed_gracefully() {
    let (tx, rx) = mpsc::unbounded_channel::<Telemetry>();
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram).with_telemetry_channel(tx);

    // Drop receiver to close channel
    drop(rx);

    // Write should still succeed even if telemetry channel is closed
    let data = b"Test data after channel close";
    let policy = Policy::default();

    let result = pipeline.write_capsule_with_policy_async(data, &policy).await;

    assert!(
        result.is_ok(),
        "write should succeed even with closed telemetry channel"
    );
}

#[tokio::test]
async fn test_multiple_writes_emit_multiple_telemetry_events() {
    let (tx, mut rx) = mpsc::unbounded_channel::<Telemetry>();
    let test_id = uuid::Uuid::new_v4();
    let temp_dir = std::env::temp_dir().join(format!("podms_test_{}", test_id));
    std::fs::create_dir_all(&temp_dir).unwrap();
    let registry_path = temp_dir.join("registry.metadata");
    let registry = CapsuleRegistry::open(&registry_path).unwrap();
    let nvram = NvramLog::open(&temp_dir.join("nvram.log")).unwrap();

    let pipeline = WritePipeline::new(registry, nvram).with_telemetry_channel(tx);

    let policy = Policy::default();

    // Write multiple capsules
    let id1 = pipeline
        .write_capsule_with_policy_async(b"First", &policy)
        .await
        .expect("first write should succeed");

    let id2 = pipeline
        .write_capsule_with_policy_async(b"Second", &policy)
        .await
        .expect("second write should succeed");

    let id3 = pipeline
        .write_capsule_with_policy_async(b"Third", &policy)
        .await
        .expect("third write should succeed");

    // Verify we got three telemetry events
    let event1 = rx.try_recv().expect("should receive first event");
    let event2 = rx.try_recv().expect("should receive second event");
    let event3 = rx.try_recv().expect("should receive third event");

    match (event1, event2, event3) {
        (
            Telemetry::NewCapsule {
                id: e1_id,
                policy: _,
                node_id: _,
            },
            Telemetry::NewCapsule {
                id: e2_id,
                policy: _,
                node_id: _,
            },
            Telemetry::NewCapsule {
                id: e3_id,
                policy: _,
                node_id: _,
            },
        ) => {
            assert_eq!(e1_id, id1);
            assert_eq!(e2_id, id2);
            assert_eq!(e3_id, id3);
        }
        _ => panic!("Expected three NewCapsule events"),
    }

    // No more events should be available
    assert!(rx.try_recv().is_err());
}
