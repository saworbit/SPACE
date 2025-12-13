use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use common::podms::{NodeId, ZoneId};
use common::{Capsule, CapsuleId, ContentHash, Policy, SegmentId};
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use scaling::compiler::{MeshState, NodeInfo, PolicyCompiler, ReplicationStrategy, ScalingAction};
use scaling::{ContentStore, MeshNode};
use tokio::sync::RwLock;
use tokio::time::sleep;

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

struct NodeHarness {
    node: Arc<MeshNode<MockContentStore>>,
    nvram: Arc<RwLock<NvramLog>>,
    addr: SocketAddr,
    _temp_dir: tempfile::TempDir,
}

impl NodeHarness {
    async fn new(port: u16) -> Result<Self> {
        let temp_dir = tempfile::tempdir()?;
        let log_path = temp_dir
            .path()
            .join(format!("nvram_resilience_{}.log", port));
        let nvram = Arc::new(RwLock::new(NvramLog::open(&log_path)?));
        let content_store = Arc::new(RwLock::new(MockContentStore::default()));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));

        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse()?;
        let zone = ZoneId::Metro {
            name: "resilience".into(),
        };

        let node = MeshNode::new(zone, addr, content_store, nvram.clone(), key_manager).await?;

        Ok(Self {
            node: Arc::new(node),
            nvram,
            addr,
            _temp_dir: temp_dir,
        })
    }
}

#[tokio::test]
async fn metro_sync_failover_retains_data_on_secondary() -> Result<()> {
    let primary = NodeHarness::new(23001).await?;
    let secondary = NodeHarness::new(23002).await?;

    secondary.node.start(vec![]).await?;
    // Register secondary as a peer for primary so replication can target it.
    primary
        .node
        .register_peer(secondary.node.id(), secondary.addr)
        .await;

    let payload = b"critical_data";
    primary
        .node
        .mirror_segment(SegmentId(77), payload, secondary.node.id())
        .await?;

    // Allow replication handler to persist the segment.
    sleep(Duration::from_millis(50)).await;

    drop(primary);

    let data = secondary.nvram.read().await.read(SegmentId(77))?;
    assert_eq!(data, payload);
    Ok(())
}

#[test]
fn force_snapshot_compiles_action_for_async_policy() {
    let mut policy = Policy::metro_sync();
    policy.rpo = Duration::from_secs(3600);

    let capsule = Capsule {
        id: CapsuleId::new(),
        size: 0,
        segments: vec![],
        created_at: 0,
        policy: policy.clone(),
        deduped_bytes: 0,
    };

    let peer = NodeId::new();
    let mesh_state = MeshState::new(
        vec![(
            peer,
            NodeInfo {
                zone: ZoneId::Metro {
                    name: "us-west".to_string(),
                },
                available_bytes: 1_000_000_000,
                used_bytes: 0,
                network_tier: scaling::NetworkTier::Standard,
            },
        )],
        ZoneId::Metro {
            name: "us-west".to_string(),
        },
    );

    let compiler = PolicyCompiler::with_defaults();
    let action = compiler.compile_immediate_replication(&capsule, None, &mesh_state);

    match action.expect("expected forced replication action") {
        ScalingAction::Replicate { strategy, .. } => match strategy {
            ReplicationStrategy::AsyncWithBatching { rpo } => {
                assert_eq!(rpo, Duration::from_secs(3600));
            }
            _ => panic!("unexpected strategy for forced snapshot"),
        },
        _ => panic!("unexpected action variant"),
    }
}
