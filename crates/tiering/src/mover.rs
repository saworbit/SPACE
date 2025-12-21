use std::path::{Path, PathBuf};

use anyhow::{anyhow, Context, Result};
use common::SegmentId;
use tokio::fs;

use crate::stub::SegmentStub;

#[derive(Debug, Clone)]
pub struct TieringPaths {
    pub hot_root: PathBuf,
    pub cold_root: PathBuf,
}

impl TieringPaths {
    pub fn segment_data_path(&self, segment: SegmentId) -> PathBuf {
        self.hot_root
            .join("segments")
            .join(format!("{}.bin", segment.0))
    }

    pub fn segment_stub_path(&self, segment: SegmentId) -> PathBuf {
        self.hot_root
            .join("segments")
            .join(format!("{}.stub.json", segment.0))
    }

    pub fn cold_object_key(&self, segment: SegmentId) -> String {
        format!("segments/{}.bin", segment.0)
    }

    pub fn cold_object_path(&self, segment: SegmentId) -> PathBuf {
        self.cold_root.join(self.cold_object_key(segment))
    }
}

async fn atomic_move_or_copy(from: &Path, to: &Path) -> Result<()> {
    if let Some(parent) = to.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    match fs::rename(from, to).await {
        Ok(()) => Ok(()),
        Err(rename_err) => {
            fs::copy(from, to)
                .await
                .with_context(|| format!("copy {} -> {}", from.display(), to.display()))?;
            fs::remove_file(from)
                .await
                .with_context(|| format!("remove {}", from.display()))?;
            tracing::debug!(error = %rename_err, "rename failed; fell back to copy+remove");
            Ok(())
        }
    }
}

pub async fn migrate_segment_to_cold(paths: &TieringPaths, segment: SegmentId) -> Result<()> {
    let data_path = paths.segment_data_path(segment);
    let stub_path = paths.segment_stub_path(segment);
    let cold_path = paths.cold_object_path(segment);

    if fs::try_exists(&stub_path).await.unwrap_or(false) {
        return Ok(());
    }
    if !fs::try_exists(&data_path).await.unwrap_or(false) {
        return Err(anyhow!("segment data not found: {}", data_path.display()));
    }

    atomic_move_or_copy(&data_path, &cold_path)
        .await
        .with_context(|| format!("migrate segment {} to cold store", segment.0))?;

    let stub = SegmentStub::new_simulated_s3(segment, paths.cold_object_key(segment));
    fs::write(&stub_path, stub.to_json_bytes()?)
        .await
        .with_context(|| format!("write stub {}", stub_path.display()))?;

    Ok(())
}

pub async fn recall_segment_from_cold(
    paths: &TieringPaths,
    segment: SegmentId,
    reheat: bool,
) -> Result<Vec<u8>> {
    let data_path = paths.segment_data_path(segment);
    if fs::try_exists(&data_path).await.unwrap_or(false) {
        return fs::read(&data_path)
            .await
            .with_context(|| format!("read segment {}", data_path.display()));
    }

    let stub_path = paths.segment_stub_path(segment);
    if !fs::try_exists(&stub_path).await.unwrap_or(false) {
        return Err(anyhow!(
            "segment {} missing: no data file or stub",
            segment.0
        ));
    }

    let stub_bytes = fs::read(&stub_path)
        .await
        .with_context(|| format!("read stub {}", stub_path.display()))?;
    let stub = SegmentStub::from_json_bytes(&stub_bytes)?;
    stub.validate_for_segment(segment)?;

    let cold_path = stub.cold_object_path(&paths.cold_root);
    let bytes = fs::read(&cold_path)
        .await
        .with_context(|| format!("read cold object {}", cold_path.display()))?;

    if reheat {
        if let Some(parent) = data_path.parent() {
            fs::create_dir_all(parent).await.ok();
        }
        fs::write(&data_path, &bytes)
            .await
            .with_context(|| format!("reheat segment {}", data_path.display()))?;
        let _ = fs::remove_file(&stub_path).await;
    }

    Ok(bytes)
}
