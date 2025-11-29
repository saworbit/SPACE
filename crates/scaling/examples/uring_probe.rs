use anyhow::Result;
use common::podms::ZoneId;
use common::{ContentHash, SegmentId};
use encryption::keymanager::KeyManager;
use nvram_sim::NvramLog;
use scaling::{ContentStore, MeshNode};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tokio::time::{sleep, Duration};
use tracing_subscriber::EnvFilter;
use uuid::Uuid;

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

async fn build_node(zone: ZoneId, addr: SocketAddr) -> Result<MeshNode<MockContentStore>> {
    let content_store = Arc::new(RwLock::new(MockContentStore::default()));
    let nvram_log = Arc::new(RwLock::new(NvramLog::open(format!(
        "test_nvram_{}.log",
        Uuid::new_v4()
    ))?));
    let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));

    MeshNode::new(zone, addr, content_store, nvram_log, key_manager).await
}

#[tokio::main(flavor = "multi_thread")]
async fn main() -> Result<()> {
    let env_filter = std::env::var("RUST_LOG").unwrap_or_else(|_| "info,scaling=debug".to_string());
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::new(env_filter))
        .compact()
        .init();

    if cfg!(not(target_os = "linux")) {
        tracing::warn!(
            "Running on non-Linux; io_uring transport is disabled and TCP fallback will be used"
        );
    }

    let zone = ZoneId::Metro {
        name: "io-uring-probe".into(),
    };
    let sender_addr: SocketAddr = "127.0.0.1:19200".parse().unwrap();
    let receiver_addr: SocketAddr = "127.0.0.1:19201".parse().unwrap();

    let sender = Arc::new(build_node(zone.clone(), sender_addr).await?);
    let receiver = Arc::new(build_node(zone.clone(), receiver_addr).await?);

    receiver.start(vec![]).await?;
    sender.register_peer(receiver.id(), receiver_addr).await;

    let frame_count: usize = std::env::var("FRAME_COUNT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256);
    let frame_bytes: usize = std::env::var("FRAME_BYTES")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(256 * 1024);

    let payload = vec![0u8; frame_bytes];

    tracing::info!(
        frames = frame_count,
        bytes_per_frame = frame_bytes,
        "starting io_uring replication probe"
    );

    for i in 0..frame_count {
        let segment_id = SegmentId(i as u64 + 1);
        sender
            .mirror_segment(segment_id, &payload, receiver.id())
            .await?;

        if i > 0 && i % 50 == 0 {
            tracing::info!(sent = i, "probe progress");
        }
    }

    // Allow queued frames to flush through the io_uring driver thread.
    sleep(Duration::from_secs(2)).await;

    tracing::info!("probe complete; inspect logs for io_uring queue depth and zero-copy path");
    Ok(())
}
