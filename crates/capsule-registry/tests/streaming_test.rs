#![cfg(feature = "modular_pipeline")]

use anyhow::Result;
use capsule_registry::modular_pipeline::{
    DefaultPolicyEvaluator, NoopEncryptor, NullKeyring, RegistryPlainPipeline,
};
use capsule_registry::CapsuleRegistry;
use common::Policy;
use compression::Lz4ZstdCompressor;
use dedup::Blake3Deduper;
use futures::StreamExt;
use std::fs;
use std::path::Path;
use storage::NvramBackend;
use uuid::Uuid;

fn test_paths(prefix: &str) -> (String, String) {
    let base = std::env::temp_dir().join("space_streaming_tests");
    let _ = fs::create_dir_all(&base);
    let unique = format!("{prefix}_{}", Uuid::new_v4());
    let log = base.join(format!("{unique}.log"));
    let meta = base.join(format!("{unique}.db"));
    (
        log.to_string_lossy().to_string(),
        meta.to_string_lossy().to_string(),
    )
}

fn cleanup(path: &str) {
    cleanup_path(path);
    cleanup_path(&format!("{path}.segments"));
}

fn cleanup_path(path: &str) {
    let p = Path::new(path);
    match fs::remove_file(p) {
        Ok(_) => (),
        Err(err) => {
            if err.kind() == std::io::ErrorKind::NotFound {
                return;
            }
            if err.kind() == std::io::ErrorKind::IsADirectory {
                let _ = fs::remove_dir_all(p);
            }
        }
    }
}

#[tokio::test]
async fn test_streaming_consistency() -> Result<()> {
    let (log_path, meta_path) = test_paths("streaming");
    cleanup(&log_path);
    cleanup(&meta_path);

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
    cleanup(&log_path);
    cleanup(&meta_path);

    Ok(())
}
