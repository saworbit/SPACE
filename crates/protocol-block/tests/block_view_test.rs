use capsule_registry::CapsuleRegistry;
use nvram_sim::NvramLog;
use protocol_block::BlockView;
use std::{
    fs,
    time::{SystemTime, UNIX_EPOCH},
};

fn unique_prefix(base: &str) -> String {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    format!("{}_{}", base, nonce)
}

fn teardown(prefix: &str) {
    let _ = fs::remove_file(format!("{}.nvram", prefix));
    let _ = fs::remove_file(format!("{}.nvram.segments", prefix));
    let _ = fs::remove_file(format!("{}.metadata", prefix));
    let _ = fs::remove_file(format!("{}.block.json", prefix));
}

fn setup(prefix: &str) -> BlockView {
    teardown(prefix);
    let log_path = format!("{}.nvram", prefix);
    let meta_path = format!("{}.metadata", prefix);
    let block_meta_path = format!("{}.block.json", prefix);
    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    BlockView::open(registry, nvram, block_meta_path).unwrap()
}

#[tokio::test]
async fn block_volume_lifecycle() {
    let prefix = unique_prefix("test_block_lifecycle");
    let block = setup(&prefix);

    let volume = block.create_volume("vol0", 16 * 1024).await.unwrap();
    assert_eq!(volume.name(), "vol0");
    assert_eq!(volume.size(), 16 * 1024);
    assert_eq!(volume.block_size(), 4096);

    // Write two sectors worth of data.
    block
        .write("vol0", 0, &[0xAA; 4096])
        .await
        .expect("first sector write");
    block
        .write("vol0", 4096, &[0x55; 4096])
        .await
        .expect("second sector write");

    let read_back = block.read("vol0", 0, 8192).await.unwrap();
    assert_eq!(&read_back[..4096], &[0xAA; 4096]);
    assert_eq!(&read_back[4096..], &[0x55; 4096]);

    let info = block.volume("vol0").await.unwrap();
    assert!(info.version() >= 3);

    let listed = block.list_volumes().await;
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].name(), "vol0");

    block.delete_volume("vol0").await.unwrap();
    assert!(block.read("vol0", 0, 4).await.is_err());

    drop(block);
    teardown(&prefix);
}

#[tokio::test]
async fn block_rejects_invalid_names() {
    let prefix = unique_prefix("test_block_invalid");
    let block = setup(&prefix);

    assert!(block.create_volume("", 4096).await.is_err());
    assert!(block.create_volume("bad/name", 4096).await.is_err());

    drop(block);
    teardown(&prefix);
}

#[tokio::test]
async fn block_persists_volumes_across_reopen() {
    let prefix = unique_prefix("test_block_persist");
    teardown(&prefix);
    let log_path = format!("{}.nvram", &prefix);
    let meta_path = format!("{}.metadata", &prefix);
    let block_meta_path = format!("{}.block.json", &prefix);

    {
        let registry = CapsuleRegistry::open(&meta_path).unwrap();
        let nvram = NvramLog::open(&log_path).unwrap();
        let block = BlockView::open(registry, nvram, &block_meta_path).unwrap();
        block.create_volume("vol", 4096).await.unwrap();
        block.write("vol", 0, &[1, 2, 3, 4]).await.unwrap();
    }

    {
        let registry = CapsuleRegistry::open(&meta_path).unwrap();
        let nvram = NvramLog::open(&log_path).unwrap();
        let block = BlockView::open(registry, nvram, &block_meta_path).unwrap();
        let listed = block.list_volumes().await;
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].name(), "vol");
        let data = block.read("vol", 0, 4).await.unwrap();
        assert_eq!(data, vec![1, 2, 3, 4]);
    }

    teardown(&prefix);
}
