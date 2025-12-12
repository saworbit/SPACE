use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{anyhow, Result};
use common::podms::ZoneId;
use common::{ContentHash, SegmentId};
use encryption::keymanager::KeyManager;
use encryption::mac;
use encryption::xts;
use futures::executor::block_on;
use nvram_sim::NvramLog;
use rand::RngCore;
use scaling::{ContentStore, MeshNode, ReplicationFrame};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::sync::RwLock;
use tokio::time::sleep;

// Lightweight mock that mirrors the crate-local test store so we can assert content lookups.
#[derive(Clone, Default)]
pub struct MockContentStore {
    store: Arc<RwLock<HashMap<ContentHash, SegmentId>>>,
}

impl ContentStore for MockContentStore {
    fn lookup_content(&self, hash: &ContentHash) -> Option<SegmentId> {
        block_on(async { self.store.read().await.get(hash).copied() })
    }

    fn register_content(&self, hash: &ContentHash, segment_id: SegmentId) {
        block_on(async {
            self.store.write().await.insert(hash.clone(), segment_id);
        });
    }
}

pub struct ReplicationTestHarness {
    pub node: Arc<MeshNode<MockContentStore>>,
    pub content_store: Arc<RwLock<MockContentStore>>,
    pub nvram: Arc<RwLock<NvramLog>>,
    pub key_manager: Arc<RwLock<KeyManager>>,
    pub addr: SocketAddr,
    _temp_dir: tempfile::TempDir,
}

impl ReplicationTestHarness {
    pub async fn new(port: u16) -> Self {
        let temp_dir = tempfile::tempdir().expect("failed to create temp dir for nvram log");
        let log_path = temp_dir.path().join(format!("test_nvram_{}.log", port));

        let content_store = Arc::new(RwLock::new(MockContentStore::default()));
        let nvram = Arc::new(RwLock::new(
            NvramLog::open(&log_path).expect("failed to open nvram log"),
        ));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));

        let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
        let zone = ZoneId::Metro {
            name: "test-zone".into(),
        };

        let node = MeshNode::new(
            zone,
            addr,
            content_store.clone(),
            nvram.clone(),
            key_manager.clone(),
        )
        .await
        .expect("failed to create mesh node");

        node.start(vec![]).await.expect("failed to start node");
        // Give the listener a moment to bind.
        sleep(Duration::from_millis(20)).await;

        Self {
            node: Arc::new(node),
            content_store,
            nvram,
            key_manager,
            addr,
            _temp_dir: temp_dir,
        }
    }

    pub async fn wait_for_segment(&self, hash: &ContentHash) -> Result<SegmentId> {
        let start = Instant::now();
        while start.elapsed() <= Duration::from_secs(1) {
            if let Some(id) = self.content_store.read().await.lookup_content(hash) {
                return Ok(id);
            }
            sleep(Duration::from_millis(10)).await;
        }

        Err(anyhow!(
            "timed out waiting for segment with hash {}",
            hash.as_str()
        ))
    }
}

async fn create_sender_node(
    port: u16,
) -> (MeshNode<MockContentStore>, SocketAddr, tempfile::TempDir) {
    let temp_dir = tempfile::tempdir().expect("failed to create sender temp dir");
    let log_path = temp_dir.path().join(format!("sender_nvram_{}.log", port));
    let content_store = Arc::new(RwLock::new(MockContentStore::default()));
    let nvram = Arc::new(RwLock::new(
        NvramLog::open(&log_path).expect("failed to open sender nvram log"),
    ));
    let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));
    let addr: SocketAddr = format!("127.0.0.1:{}", port).parse().unwrap();
    let zone = ZoneId::Metro {
        name: format!("sender-zone-{}", port),
    };

    let node = MeshNode::new(zone, addr, content_store, nvram, key_manager)
        .await
        .expect("failed to build sender node");

    (node, addr, temp_dir)
}

fn content_hash(data: &[u8]) -> ContentHash {
    ContentHash::from_bytes(blake3::hash(data).as_bytes())
}

async fn build_tampered_frame(
    harness: &ReplicationTestHarness,
    payload: &[u8],
    segment_id: SegmentId,
) -> Result<Vec<u8>> {
    let mut key_manager = harness.key_manager.write().await;
    let key_pair = key_manager.get_key(1)?.clone();
    drop(key_manager);

    let tweak = xts::derive_tweak_from_hash(blake3::hash(payload).as_bytes());
    let (ciphertext, metadata) = xts::encrypt_segment(payload, &key_pair, 1, tweak)?;
    let mac_tag = mac::compute_mac(&ciphertext, &metadata, key_pair.key1(), key_pair.key2())?;

    let mut metadata_with_mac = metadata;
    metadata_with_mac.set_integrity_tag(mac_tag);

    let mut tampered_ciphertext = ciphertext;
    if let Some(first) = tampered_ciphertext.first_mut() {
        *first ^= 0x01;
    }

    let frame = ReplicationFrame::new(segment_id, metadata_with_mac, tampered_ciphertext);
    frame.to_bytes()
}

#[tokio::test]
async fn test_e2e_replication_persistence() {
    let harness = ReplicationTestHarness::new(21001).await;
    let (sender, sender_addr, _sender_dir) = create_sender_node(21002).await;

    harness.node.register_peer(sender.id(), sender_addr).await;
    sender.register_peer(harness.node.id(), harness.addr).await;

    let payload = b"integrity_check_payload";
    sender
        .mirror_segment(SegmentId(1), payload, harness.node.id())
        .await
        .unwrap();

    let hash = content_hash(payload);
    let seg_id = harness
        .wait_for_segment(&hash)
        .await
        .expect("segment not persisted");

    let log = harness.nvram.read().await;
    let stored_data = log.read(seg_id).unwrap();
    assert_eq!(stored_data, payload);
}

#[tokio::test]
async fn test_deduplication_refcounting() {
    let harness = ReplicationTestHarness::new(21003).await;
    let (sender, sender_addr, _sender_dir) = create_sender_node(21004).await;

    harness.node.register_peer(sender.id(), sender_addr).await;
    sender.register_peer(harness.node.id(), harness.addr).await;

    let payload = b"dedup_payload_block";
    let hash = content_hash(payload);

    sender
        .mirror_segment(SegmentId(100), payload, harness.node.id())
        .await
        .unwrap();
    let seg_id = harness
        .wait_for_segment(&hash)
        .await
        .expect("first segment not persisted");

    {
        let log = harness.nvram.read().await;
        let segments = log.list_segments().unwrap();
        assert_eq!(segments.len(), 1);
        let segment = segments.iter().find(|s| s.id == seg_id).unwrap();
        assert_eq!(segment.ref_count, 1);
    }

    sender
        .mirror_segment(SegmentId(101), payload, harness.node.id())
        .await
        .unwrap();
    // Allow the handler to process the dedup hit.
    sleep(Duration::from_millis(50)).await;

    {
        let log = harness.nvram.read().await;
        let segments = log.list_segments().unwrap();
        assert_eq!(segments.len(), 1);
        let segment = segments.iter().find(|s| s.id == seg_id).unwrap();
        assert_eq!(segment.ref_count, 2);
        assert!(segment.deduplicated);
    }

    let mapped = harness
        .content_store
        .read()
        .await
        .lookup_content(&hash)
        .expect("hash should remain mapped");
    assert_eq!(mapped, seg_id);
}

#[tokio::test]
async fn test_reject_tampered_mac() {
    let harness = ReplicationTestHarness::new(21005).await;
    let payload = b"tampered_payload_block_data";

    let frame_bytes = build_tampered_frame(&harness, payload, SegmentId(5))
        .await
        .expect("failed to build tampered frame");

    let mut stream = TcpStream::connect(harness.addr).await.unwrap();
    stream.write_all(&frame_bytes).await.unwrap();
    stream.shutdown().await.unwrap();

    sleep(Duration::from_millis(100)).await;

    let log = harness.nvram.read().await;
    assert!(log.list_segments().unwrap().is_empty());

    let hash = content_hash(payload);
    assert!(harness
        .content_store
        .read()
        .await
        .lookup_content(&hash)
        .is_none());
}

#[tokio::test]
async fn test_ignore_garbage_frames() {
    let harness = ReplicationTestHarness::new(21006).await;

    let mut fuzz_bytes = vec![0u8; 128];
    rand::thread_rng().fill_bytes(&mut fuzz_bytes);

    let mut stream = TcpStream::connect(harness.addr).await.unwrap();
    stream.write_all(&fuzz_bytes).await.unwrap();
    stream.shutdown().await.unwrap();

    sleep(Duration::from_millis(100)).await;

    let log = harness.nvram.read().await;
    assert!(log.list_segments().unwrap().is_empty());

    let store = harness.content_store.read().await;
    assert!(store.store.read().await.is_empty());
}
