//! Comprehensive tests for PODMS scaling module

use crate::{ContentStore, ReplicationFrame, ReplicationHandler};
use common::{ContentHash, SegmentId};
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{Barrier, Mutex, RwLock};

// Mock ContentStore for testing
#[derive(Clone, Default)]
struct MockContentStore {
    store: Arc<RwLock<HashMap<ContentHash, SegmentId>>>,
}

impl ContentStore for MockContentStore {
    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        futures::executor::block_on(async { self.store.read().await.get(hash).copied() })
    }

    fn register_content(&self, hash: &ContentHash, segment_id: SegmentId) {
        futures::executor::block_on(async {
            self.store.write().await.insert(hash.clone(), segment_id);
        });
    }
}

// Helper to create test MeshNode with mocks
async fn create_test_mesh_node(
    zone: common::podms::ZoneId,
    addr: std::net::SocketAddr,
) -> anyhow::Result<crate::MeshNode<MockContentStore>> {
    let content_store = Arc::new(RwLock::new(MockContentStore::default()));
    let nvram_log = Arc::new(RwLock::new(NvramLog::open(format!(
        "test_nvram_{}.log",
        uuid::Uuid::new_v4()
    ))?));
    // Create a test master key (all zeros for testing)
    let master_key = [0u8; 32];
    let key_manager = Arc::new(RwLock::new(KeyManager::new(master_key)));

    crate::MeshNode::new(zone, addr, content_store, nvram_log, key_manager).await
}

#[cfg(test)]
mod mesh_tests {
    use super::*;
    use crate::NetworkTier;
    use common::podms::ZoneId;
    use std::sync::Arc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_mesh_node_lifecycle() {
        let zone = ZoneId::Metro {
            name: "us-west-1a".into(),
        };
        let addr = "127.0.0.1:19000".parse().unwrap();

        let node = create_test_mesh_node(zone.clone(), addr).await.unwrap();
        assert_eq!(node.zone(), &zone);
        assert!(node.capabilities().has_nvram);
        assert_eq!(
            node.capabilities().network_tier as u8,
            NetworkTier::Standard as u8
        );
    }

    #[tokio::test]
    async fn test_peer_registration_and_lookup() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };
        let addr = "127.0.0.1:19001".parse().unwrap();
        let node = create_test_mesh_node(zone, addr).await.unwrap();

        // Register multiple peers
        let peer1_id = common::podms::NodeId::new();
        let peer1_addr = "127.0.0.1:19002".parse().unwrap();
        let peer2_id = common::podms::NodeId::new();
        let peer2_addr = "127.0.0.1:19003".parse().unwrap();

        node.register_peer(peer1_id, peer1_addr).await;
        node.register_peer(peer2_id, peer2_addr).await;

        let peers = node.peers.read().await;
        assert_eq!(peers.len(), 2);
        assert_eq!(peers.get(&peer1_id), Some(&peer1_addr));
        assert_eq!(peers.get(&peer2_id), Some(&peer2_addr));
    }

    #[tokio::test]
    async fn test_mirror_segment_requires_registered_peer() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };
        let addr = "127.0.0.1:19004".parse().unwrap();
        let node = create_test_mesh_node(zone, addr).await.unwrap();

        let unknown_peer = common::podms::NodeId::new();
        let data = b"test segment data";

        // Should fail: peer not registered
        let result = node
            .mirror_segment(common::SegmentId(1), data, unknown_peer)
            .await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("not found"));
    }

    #[tokio::test]
    async fn test_mirror_segment_basic() {
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };

        // Create two nodes
        let node1_addr = "127.0.0.1:19005".parse().unwrap();
        let node1 = Arc::new(
            create_test_mesh_node(zone.clone(), node1_addr)
                .await
                .unwrap(),
        );

        let node2_addr = "127.0.0.1:19006".parse().unwrap();
        let node2 = Arc::new(
            create_test_mesh_node(zone.clone(), node2_addr)
                .await
                .unwrap(),
        );

        // Start node2 to accept mirrors
        node2.start(vec![]).await.unwrap();

        // Give listener time to bind
        sleep(Duration::from_millis(100)).await;

        // Register node2 as peer of node1
        node1.register_peer(node2.id(), node2_addr).await;

        // Mirror data from node1 to node2
        let test_data = b"test segment for mirroring";
        let result = node1
            .mirror_segment(common::SegmentId(42), test_data, node2.id())
            .await;

        // Should succeed
        assert!(result.is_ok());
    }
}

#[cfg(test)]
mod agent_tests {
    use super::*;
    use crate::agent::ScalingAgent;
    use common::podms::{Telemetry, ZoneId};
    use common::{CapsuleId, Policy};
    use std::sync::Arc;
    use tokio::sync::mpsc;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn test_agent_handles_new_capsule_event() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19100".parse().unwrap();
        let mesh_node = Arc::new(create_test_mesh_node(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node);

        let (tx, rx) = mpsc::unbounded_channel();

        // Spawn agent in background
        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send a metro-sync capsule event
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        tx.send(Telemetry::NewCapsule {
            id: capsule_id,
            policy: policy.clone(),
            node_id: None,
        })
        .unwrap();

        // Give agent time to process
        sleep(Duration::from_millis(50)).await;

        // Close channel to shut down agent
        drop(tx);

        // Wait for agent to finish
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_heat_spike() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19101".parse().unwrap();
        let mesh_node = Arc::new(create_test_mesh_node(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node);

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send heat spike event
        tx.send(Telemetry::HeatSpike {
            id: CapsuleId::new(),
            accesses_per_min: 10000,
            node_id: None,
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_capacity_threshold() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19102".parse().unwrap();
        let mesh_node = Arc::new(create_test_mesh_node(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node.clone());

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send capacity threshold event
        tx.send(Telemetry::CapacityThreshold {
            node_id: mesh_node.id(),
            used_bytes: 900_000_000_000,
            total_bytes: 1_000_000_000_000,
            threshold_pct: 0.9,
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn test_agent_handles_node_degraded() {
        let zone = ZoneId::Metro {
            name: "test".into(),
        };
        let addr = "127.0.0.1:19103".parse().unwrap();
        let mesh_node = Arc::new(create_test_mesh_node(zone, addr).await.unwrap());

        let agent = ScalingAgent::new(mesh_node.clone());

        let (tx, rx) = mpsc::unbounded_channel();

        let agent_handle = tokio::spawn(async move { agent.run(rx).await });

        // Send node degraded event
        tx.send(Telemetry::NodeDegraded {
            node_id: mesh_node.id(),
            reason: "disk failure detected".into(),
        })
        .unwrap();

        sleep(Duration::from_millis(50)).await;
        drop(tx);
        agent_handle.await.unwrap().unwrap();
    }
}

#[cfg(test)]
mod transport_tests {
    use super::*;
    use crate::{DataTransport, TcpTransport};
    use common::podms::NodeId;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;
    use tokio::sync::oneshot;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn tcp_transport_reconnects_after_disconnect() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let recv_clone = received.clone();
        let (first_done_tx, first_done_rx) = oneshot::channel();

        tokio::spawn(async move {
            let payloads = [vec![1u8, 2, 3], vec![4u8, 5, 6, 7]];
            let mut first_done_tx = Some(first_done_tx);

            for (idx, payload) in payloads.into_iter().enumerate() {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut buf = vec![0u8; payload.len()];
                socket.read_exact(&mut buf).await.unwrap();
                recv_clone.lock().await.push(buf);

                if idx == 0 {
                    if let Some(tx) = first_done_tx.take() {
                        let _ = tx.send(());
                    }
                }

                let _ = socket.shutdown().await;
                // Drop socket to force the client to reconnect on the next send.
            }
        });

        let transport = TcpTransport::new();
        let node = NodeId::new();

        transport
            .send_frame(node, addr, vec![1u8, 2, 3])
            .await
            .unwrap();

        // Wait for the server to close the first connection.
        first_done_rx.await.unwrap();

        // Simulate a broken write side before attempting the next send.
        transport.connections.shutdown_writer(node).await;

        transport
            .send_frame(node, addr, vec![4u8, 5, 6, 7])
            .await
            .unwrap();

        // Allow the server to read the second frame.
        sleep(Duration::from_millis(50)).await;

        let received = received.lock().await;
        assert_eq!(received.len(), 2);
        assert_eq!(received[0], vec![1u8, 2, 3]);
        assert_eq!(received[1], vec![4u8, 5, 6, 7]);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod io_uring_transport_tests {
    use super::*;
    use crate::IoUringTransport;
    use common::podms::NodeId;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;
    use tokio::net::TcpListener;
    use tokio::time::{sleep, Duration};

    #[tokio::test]
    async fn io_uring_transport_reuses_connection() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let received = Arc::new(Mutex::new(Vec::new()));
        let accept_count = Arc::new(AtomicUsize::new(0));
        let recv_clone = received.clone();
        let accept_clone = accept_count.clone();

        tokio::spawn(async move {
            let expected_lengths = [3usize, 4usize];
            let mut total = 0usize;

            while total < expected_lengths.len() {
                let (mut socket, _) = listener.accept().await.unwrap();
                accept_clone.fetch_add(1, Ordering::SeqCst);

                for expected_len in expected_lengths[total..].iter().copied() {
                    let mut buf = vec![0u8; expected_len];
                    match socket.read_exact(&mut buf).await {
                        Ok(_) => {
                            recv_clone.lock().await.push(buf);
                            total += 1;
                            if total >= expected_lengths.len() {
                                return;
                            }
                        }
                        Err(_) => break,
                    }
                }
            }
        });

        let transport = IoUringTransport::new();
        let node = NodeId::new();

        transport
            .send_frame(node, addr, vec![1u8, 2, 3])
            .await
            .unwrap();
        transport
            .send_frame(node, addr, vec![4u8, 5, 6, 7])
            .await
            .unwrap();

        sleep(Duration::from_millis(50)).await;

        let received = received.lock().await;
        assert_eq!(accept_count.load(Ordering::SeqCst), 1);
        assert_eq!(received.len(), 2);
        assert_eq!(received[0], vec![1u8, 2, 3]);
        assert_eq!(received[1], vec![4u8, 5, 6, 7]);
    }
}

#[cfg(test)]
mod replication_tests {
    use super::*;
    use encryption::policy::EncryptionMetadata;

    #[tokio::test]
    async fn inflight_registry_deduplicates_concurrent_segments() {
        let content_store = Arc::new(RwLock::new(MockContentStore::default()));
        let log_path = format!("test_nvram_dedup_{}.log", uuid::Uuid::new_v4());
        let nvram_log = Arc::new(RwLock::new(NvramLog::open(&log_path).unwrap()));
        let master_key = [0u8; 32];
        let key_manager = Arc::new(RwLock::new(KeyManager::new(master_key)));
        let handler = Arc::new(ReplicationHandler::new(
            content_store.clone(),
            nvram_log.clone(),
            key_manager,
        ));

        let payload = b"concurrent payload".to_vec();
        let mut metadata = EncryptionMetadata::new_unencrypted();
        metadata.ciphertext_len = Some(payload.len() as u32);
        let frame = ReplicationFrame::new(SegmentId(7), metadata, payload.clone());

        let concurrency = 10;
        let barrier = Arc::new(Barrier::new(concurrency));
        let mut tasks = Vec::new();

        for _ in 0..concurrency {
            let handler = handler.clone();
            let frame = frame.clone();
            let barrier = barrier.clone();
            tasks.push(tokio::spawn(async move {
                barrier.wait().await;
                handler.process_segment(frame).await
            }));
        }

        for task in tasks {
            task.await.unwrap().unwrap();
        }

        let content_hash = ContentHash::from_bytes(blake3::hash(&payload).as_bytes());
        let existing = content_store
            .read()
            .await
            .lookup_content(&content_hash)
            .expect("content should be registered");

        let segments = nvram_log.read().await.list_segments().unwrap();
        assert_eq!(segments.len(), 1);
        assert_eq!(segments[0].id, existing);
        assert_eq!(segments[0].ref_count, concurrency as u32);

        let _ = std::fs::remove_file(&log_path);
        let _ = std::fs::remove_file(format!("{}.segments", log_path));
    }
}
