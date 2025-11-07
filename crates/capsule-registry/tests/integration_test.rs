use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use nvram_sim::NvramLog;
use std::fs;

#[test]
fn test_write_and_read_capsule() {
    let log_path = "test_nvram.log";
    let meta_path = "test_nvram.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"Hello SPACE! This is capsule test data.";
    let capsule_id = pipeline.write_capsule(test_data).unwrap();

    println!("Created capsule: {:?}", capsule_id);

    let read_data = pipeline.read_capsule(capsule_id).unwrap();
    assert_eq!(test_data.as_slice(), read_data.as_slice());
    println!("Write/Read test passed!");

    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[test]
fn test_compression_integration() {
    let log_path = "test_compression.log";
    let meta_path = "test_compression.metadata";
    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);

    let registry = CapsuleRegistry::open(meta_path).unwrap();
    let nvram = NvramLog::open(log_path).unwrap();
    let pipeline = WritePipeline::new(registry, nvram);

    let test_data = b"SPACE ".repeat(10_000);
    let capsule_id = pipeline.write_capsule(&test_data).unwrap();
    let read_data = pipeline.read_capsule(capsule_id).unwrap();

    assert_eq!(test_data, read_data);
    println!("Compression integration test passed!");

    let _ = fs::remove_file(log_path);
    let _ = fs::remove_file(format!("{}.segments", log_path));
    let _ = fs::remove_file(meta_path);
}

#[cfg(feature = "modular_pipeline")]
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
    use futures::executor::block_on;
    use nvram_sim::NvramLog;
    use pipeline::Pipeline as ModularPipeline;
    use storage::NvramBackend;
    use std::sync::{Arc, Mutex};

    #[test]
    fn modular_pipeline_write_succeeds() {
        let mut pipeline: capsule_registry::modular_pipeline::InMemoryPipeline =
            PipelineBuilder::new().build();
        let policy = Policy::default();

        block_on(pipeline.write_capsule(b"modular integration payload", &policy)).unwrap();
        block_on(pipeline.write_capsule(b"modular integration payload", &policy)).unwrap();

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

    #[test]
    fn modular_pipeline_encryption_sets_metadata() {
        let log_path = "modular_pipeline_integration.log";
        let segments_path = format!("{}.segments", log_path);
        let _ = fs::remove_file(log_path);
        let _ = fs::remove_file(&segments_path);

        let storage = NvramBackend::open(log_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0xA5; MASTER_KEY_SIZE])));

        let encryptor = XtsEncryptor::new(Arc::clone(&key_manager));
        let keyring = KeyManagerKeyring::new(key_manager);
        let mut pipeline = ModularPipeline::new(
            Lz4ZstdCompressor::default(),
            Blake3Deduper::default(),
            encryptor,
            storage.clone(),
            DefaultPolicyEvaluator::default(),
            Some(keyring),
            pipeline::InMemoryCatalog::default(),
        );

        let policy = Policy::encrypted();
        block_on(pipeline.write_capsule(b"encrypted modular integration data", &policy)).unwrap();

        // Re-open the log to inspect persisted metadata.
        let log = nvram_sim::NvramLog::open(log_path).unwrap();
        let segment = log.get_segment_metadata(SegmentId(0)).unwrap();

        assert!(segment.encrypted, "segment should be marked encrypted");
        assert!(
            segment.integrity_tag.is_some(),
            "integrity tag should be recorded for encrypted segment"
        );

        let _ = fs::remove_file(log_path);
        let _ = fs::remove_file(segments_path);
    }

    #[test]
    fn registry_pipeline_lifecycle() {
        let log_path = "modular_registry_lifecycle.log".to_string();
        let segments_path = format!("{}.segments", log_path);
        let meta_path = "modular_registry_lifecycle.metadata";
        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(&segments_path);
        let _ = fs::remove_file(meta_path);

        let registry = CapsuleRegistry::open(meta_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0x5Au8; MASTER_KEY_SIZE])));

        let mut pipeline = registry_nvram_pipeline_with_encryption(
            &log_path,
            registry.clone(),
            Arc::clone(&key_manager),
        )
        .unwrap();

        let policy = Policy::default();
        let capsule_id =
            block_on(pipeline.write_capsule(b"registry lifecycle data", &policy)).unwrap();
        let read_back = block_on(pipeline.read_capsule(capsule_id)).unwrap();
        assert_eq!(read_back, b"registry lifecycle data");

        block_on(pipeline.delete_capsule(capsule_id)).unwrap();
        assert!(block_on(pipeline.read_capsule(capsule_id)).is_err());

        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(segments_path);
        let _ = fs::remove_file(meta_path);
    }

    #[test]
    fn registry_pipeline_garbage_collects_orphans() {
        let log_path = "modular_registry_gc.log".to_string();
        let segments_path = format!("{}.segments", log_path);
        let meta_path = "modular_registry_gc.metadata";
        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(&segments_path);
        let _ = fs::remove_file(meta_path);

        let registry = CapsuleRegistry::open(meta_path).unwrap();
        let key_manager = Arc::new(Mutex::new(KeyManager::new([0x4Bu8; MASTER_KEY_SIZE])));

        let mut pipeline = registry_nvram_pipeline_with_encryption(
            &log_path,
            registry.clone(),
            Arc::clone(&key_manager),
        )
        .unwrap();

        let policy = Policy::default();
        let capsule_id =
            block_on(pipeline.write_capsule(b"orphaned data block", &policy)).unwrap();
        drop(pipeline);

        let capsule = registry.lookup(capsule_id).unwrap();
        let seg_id = capsule.segments[0];
        let log = NvramLog::open(&log_path).unwrap();
        let mut metadata = log.get_segment_metadata(seg_id).unwrap();
        metadata.ref_count = 0;
        metadata.deduplicated = false;
        log.update_segment_metadata(seg_id, metadata.clone()).unwrap();
        registry.delete_capsule(capsule_id).unwrap();
        drop(log);

        let mut pipeline = registry_nvram_pipeline_with_encryption(
            &log_path,
            registry.clone(),
            key_manager,
        )
        .unwrap();

        let reclaimed = block_on(pipeline.garbage_collect()).unwrap();
        assert_eq!(reclaimed, 1);
        let log = NvramLog::open(&log_path).unwrap();
        assert!(log.get_segment_metadata(seg_id).is_err());
        if let Some(hash) = metadata.content_hash {
            assert!(registry.lookup_content(&hash).is_none());
        }

        let _ = fs::remove_file(&log_path);
        let _ = fs::remove_file(segments_path);
        let _ = fs::remove_file(meta_path);
    }
}
