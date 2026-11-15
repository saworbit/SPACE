use anyhow::Result;
use capsule_registry::modular_pipeline::{
    DefaultPolicyEvaluator, NoopEncryptor, NullKeyring, RegistryPlainPipeline,
};
use capsule_registry::CapsuleRegistry;
use common::Policy;
use compression::Lz4ZstdCompressor;
use dedup::Blake3Deduper;
use futures::StreamExt;
use storage::NvramBackend;
use test_support::TempDir;

/// A scratch directory, and the log and registry paths inside it.
fn test_paths(prefix: &str) -> (TempDir, String, String) {
    let dir = TempDir::new(&format!("space_streaming_{prefix}"));
    let log = dir.join("space.log").to_string_lossy().into_owned();
    let meta = dir.join("space.db").to_string_lossy().into_owned();
    (dir, log, meta)
}

#[tokio::test]
async fn test_streaming_consistency() -> Result<()> {
    let (_dir, log_path, meta_path) = test_paths("streaming");

    let registry = CapsuleRegistry::open(&meta_path)?;
    let storage = NvramBackend::open(&log_path)?;
    let mut pipeline: RegistryPlainPipeline = RegistryPlainPipeline::new(
        Lz4ZstdCompressor,
        Blake3Deduper::default(),
        NoopEncryptor,
        storage,
        DefaultPolicyEvaluator,
        None::<NullKeyring>,
        registry,
    );

    let data_size = 5 * 1024 * 1024;
    let original_data: Vec<u8> = (0..data_size).map(|i| (i % 255) as u8).collect();
    let policy = Policy::default();

    let id = pipeline.write_capsule(&original_data, &policy).await?;

    let mut stream = pipeline.read_capsule_stream(id).await?;
    let mut accumulated_data = Vec::new();

    while let Some(chunk_res) = stream.next().await {
        let chunk = chunk_res?;
        accumulated_data.extend_from_slice(&chunk);
        assert!(accumulated_data.len() <= data_size);
    }

    assert_eq!(original_data.len(), accumulated_data.len());
    assert_eq!(original_data, accumulated_data);

    drop(stream);
    drop(pipeline);

    Ok(())
}
