use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use nvram_sim::NvramLog;
use test_support::TempDir;

#[tokio::test]
async fn test_write_and_read_capsule() {
    let dir = TempDir::new("space_integration_test_nvram");
    let log_path = dir.join("space.log");
    let meta_path = dir.join("space.metadata");

    let registry = CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).unwrap();
    let nvram = NvramLog::open(log_path.to_string_lossy().as_ref()).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"Hello SPACE! This is capsule test data.";
    let capsule_id = pipeline.write_capsule(test_data).await.unwrap();

    println!("Created capsule: {:?}", capsule_id);

    let read_data = pipeline.read_capsule(capsule_id).await.unwrap();
    assert_eq!(test_data.as_slice(), read_data.as_slice());
    println!("Write/Read test passed!");
}

#[tokio::test]
async fn test_compression_integration() {
    let dir = TempDir::new("space_integration_test_compression");
    let log_path = dir.join("space.log");
    let meta_path = dir.join("space.metadata");

    let registry = CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).unwrap();
    let nvram = NvramLog::open(log_path.to_string_lossy().as_ref()).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"SPACE ".repeat(10_000);
    let capsule_id = pipeline.write_capsule(&test_data).await.unwrap();
    let read_data = pipeline.read_capsule(capsule_id).await.unwrap();

    assert_eq!(test_data, read_data);
    println!("Compression integration test passed!");
}

#[tokio::test]
async fn hybrid_kyber_with_dedup_is_rejected_at_pipeline_boundary() {
    use common::{CryptoProfile, Policy};

    let dir = TempDir::new("space_integration_test_kyber_dedup");
    let log_path = dir.join("space.log");
    let meta_path = dir.join("space.metadata");

    let registry = CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).unwrap();
    let nvram = NvramLog::open(log_path.to_string_lossy().as_ref()).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let policy = Policy {
        crypto_profile: CryptoProfile::HybridKyber,
        dedupe: true,
        ..Policy::default()
    };

    let err = pipeline
        .write_capsule_with_policy(b"unused", &policy)
        .await
        .expect_err("HybridKyber must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("HybridKyber") && msg.contains("not supported"),
        "error must say HybridKyber isn't supported; got: {msg}"
    );
}

mod modular_pipeline_integration {
    use super::*;
    use capsule_registry::modular_pipeline::{
        registry_nvram_pipeline_with_encryption, DefaultPolicyEvaluator, KeyManagerKeyring,
        PipelineBuilder, XtsEncryptor,
    };
    use capsule_registry::CapsuleRegistry;
    use common::{Policy, SegmentId};
    use compression::Lz4ZstdCompressor;
    use dedup::Blake3Deduper;
    use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
    use nvram_sim::NvramLog;
    use pipeline::Pipeline as ModularPipeline;
    use std::sync::{Arc, Mutex};
    use storage::NvramBackend;

    #[tokio::test]
    async fn modular_pipeline_write_succeeds() {
        let mut pipeline: capsule_registry::modular_pipeline::InMemoryPipeline =
            PipelineBuilder::new().build();
        let policy = Policy::default();

        pipeline
            .write_capsule(b"modular integration payload", &policy)
            .await
            .unwrap();
        pipeline
            .write_capsule(b"modular integration payload", &policy)
            .await
            .unwrap();

        let stats = pipeline.stats();
        assert!(
            stats.total_segments >= 1,
            "modular pipeline should record segments after writes"
        );
        assert!(
            stats.deduped_segments >= 1,
            "second write should register a dedup hit"
        );
    }

    #[tokio::test]
    async fn modular_pipeline_encryption_sets_metadata() {
        let dir = TempDir::new("space_integration_modular");
        let log_path = dir.join("modular_pipeline_integration.log");

        let storage = NvramBackend::open(&log_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0xA5; MASTER_KEY_SIZE])));

        let encryptor = XtsEncryptor::new(Arc::clone(&key_manager));
        let keyring = KeyManagerKeyring::new(key_manager);
        let mut pipeline = ModularPipeline::new(
            Lz4ZstdCompressor,
            Blake3Deduper::default(),
            encryptor,
            storage.clone(),
            DefaultPolicyEvaluator,
            Some(keyring),
            pipeline::InMemoryCatalog::default(),
        );

        let policy = Policy::encrypted();
        pipeline
            .write_capsule(b"encrypted modular integration data", &policy)
            .await
            .unwrap();

        // Re-open the log to inspect persisted metadata.
        let log = nvram_sim::NvramLog::open(&log_path).unwrap();
        let segment = log.get_segment_metadata(SegmentId(0)).unwrap();

        assert!(segment.encrypted, "segment should be marked encrypted");
        assert!(
            segment.integrity_tag.is_some(),
            "integrity tag should be recorded for encrypted segment"
        );
    }

    #[tokio::test]
    async fn registry_pipeline_lifecycle() {
        // A fresh directory per run: a journal left by an earlier run would
        // belong to another store than this run's new data log.
        let dir = TempDir::new("space_integration_lifecycle");
        let log_path = dir.join("space.nvram").to_string_lossy().to_string();
        let meta_string = dir.join("space.db").to_string_lossy().to_string();
        let meta_path = meta_string.as_str();

        let registry = CapsuleRegistry::open(meta_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0x5Au8; MASTER_KEY_SIZE])));

        let mut pipeline = registry_nvram_pipeline_with_encryption(
            &log_path,
            registry.clone(),
            Arc::clone(&key_manager),
        )
        .unwrap();

        let policy = Policy::default();
        let capsule_id = pipeline
            .write_capsule(b"registry lifecycle data", &policy)
            .await
            .unwrap();
        let read_back = pipeline.read_capsule(capsule_id).await.unwrap();
        assert_eq!(read_back, b"registry lifecycle data");

        pipeline.delete_capsule(capsule_id).await.unwrap();
        assert!(pipeline.read_capsule(capsule_id).await.is_err());
    }

    #[tokio::test]
    async fn registry_pipeline_garbage_collects_orphans() {
        // A fresh directory per run: a journal left by an earlier run would
        // belong to another store than this run's new data log.
        let dir = TempDir::new("space_integration_gc");
        let log_path = dir.join("space.nvram").to_string_lossy().to_string();
        let meta_string = dir.join("space.db").to_string_lossy().to_string();
        let meta_path = meta_string.as_str();

        let registry = CapsuleRegistry::open(meta_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0x4Bu8; MASTER_KEY_SIZE])));

        let mut pipeline = registry_nvram_pipeline_with_encryption(
            &log_path,
            registry.clone(),
            Arc::clone(&key_manager),
        )
        .unwrap();

        let policy = Policy::default();
        let capsule_id = pipeline
            .write_capsule(b"orphaned data block", &policy)
            .await
            .unwrap();
        drop(pipeline);

        // Drop the capsule record without releasing its segment, as a crash
        // between the two would.
        let capsule = registry.lookup(capsule_id).unwrap();
        let seg_id = capsule.segments[0];
        registry.delete_capsule(capsule_id).unwrap();

        // Opening a pipeline recounts references, so the segment is an
        // orphan with none.
        let log = NvramLog::open(&log_path).unwrap();
        let mut pipeline = capsule_registry::modular_pipeline::registry_pipeline_from_log_with_key(
            log.clone(),
            registry.clone(),
            key_manager,
        )
        .unwrap();
        let metadata = log.get_segment_metadata(seg_id).unwrap();
        assert_eq!(metadata.ref_count, 0);

        let reclaimed = pipeline.garbage_collect().await.unwrap();
        assert_eq!(reclaimed, 1);
        assert!(log.get_segment_metadata(seg_id).is_err());
        if let Some(hash) = metadata.content_hash {
            assert!(registry.lookup_content(&hash).is_none());
        }

        // The record stays in the file until compaction drops it.
        drop(pipeline);
        log.compact().unwrap();
        drop(log);
        let log = NvramLog::open(&log_path).unwrap();
        assert!(log.get_segment_metadata(seg_id).is_err());
    }
}

#[tokio::test]
async fn write_capsule_encrypts_by_default_when_a_key_is_configured() {
    let dir = TempDir::new("space_integration_default_policy");
    let log_path = dir.join("space.log");
    let meta_path = dir.join("space.metadata");

    let registry = CapsuleRegistry::open(&meta_path).unwrap();
    let nvram = NvramLog::open(&log_path).unwrap();
    let key_manager = encryption::KeyManager::new([7u8; 32]);
    let pipeline = WritePipeline::with_key_manager(registry.clone(), nvram, key_manager);

    let secret = b"TOP-SECRET-MARKER\n";
    let id = pipeline.write_capsule(secret).await.unwrap();
    assert!(registry.lookup(id).unwrap().policy.encryption.is_enabled());
    assert_eq!(pipeline.read_capsule(id).await.unwrap(), secret);

    let stored = std::fs::read(&log_path).unwrap();
    assert!(
        !stored.windows(secret.len()).any(|w| w == secret),
        "plaintext found in the log"
    );

    drop(pipeline);
}
