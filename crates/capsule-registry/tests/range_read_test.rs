use anyhow::Result;
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use nvram_sim::NvramLog;
use test_support::TempDir;

#[tokio::test]
async fn test_read_range_logic() -> Result<()> {
    let dir = TempDir::new("space_range_read");
    let log_path = dir.join("range.log");
    let meta_path = dir.join("range.db");

    let registry = CapsuleRegistry::open(meta_path.to_string_lossy().as_ref())?;
    let nvram = NvramLog::open(log_path.to_string_lossy().as_ref())?;
    let pipeline = WritePipeline::new(registry, nvram);

    let data: Vec<u8> = (0u8..=255).collect();
    let capsule_id = pipeline.write_capsule(&data).await?;

    // 1. Test Start Range
    let start_slice = pipeline.read_range(capsule_id, 0, 10).await?;
    assert_eq!(start_slice.len(), 10);
    assert_eq!(start_slice, (0u8..10).collect::<Vec<u8>>());

    // 2. Test Middle Range
    let mid_slice = pipeline.read_range(capsule_id, 100, 5).await?;
    assert_eq!(mid_slice.len(), 5);
    assert_eq!(mid_slice, vec![100, 101, 102, 103, 104]);

    // 3. Test End Range (Saturating)
    let end_slice = pipeline.read_range(capsule_id, 250, 10).await?;
    assert_eq!(end_slice.len(), 6);
    assert_eq!(end_slice, vec![250, 251, 252, 253, 254, 255]);

    // 4. Test Out of Bounds
    let oob_slice = pipeline.read_range(capsule_id, 300, 10).await?;
    assert_eq!(oob_slice.len(), 0);
    Ok(())
}
