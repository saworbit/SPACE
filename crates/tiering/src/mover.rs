use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use bytes::Bytes;
use common::SegmentId;
use object_store::path::Path as ObjPath;
use object_store::ObjectStore;
use sha2::{Digest, Sha256};
use tokio::fs;

use crate::stub::{is_stub_bytes, make_stub, object_path_from_remote_url, parse_stub};

#[derive(Debug, Clone)]
pub struct TieringPaths {
    pub hot_root: PathBuf,
    pub cold_store: Arc<dyn ObjectStore>,
    /// Bucket name used for formatting `s3://...` stub URLs (ignored by `object_store` APIs).
    pub cold_bucket: String,
}

impl TieringPaths {
    pub fn simulated_s3(hot_root: PathBuf, cold_root: PathBuf) -> Result<Self> {
        Self::simulated_s3_with_bucket(hot_root, cold_root, "bucket")
    }

    pub fn simulated_s3_with_bucket(
        hot_root: PathBuf,
        cold_root: PathBuf,
        bucket: impl Into<String>,
    ) -> Result<Self> {
        std::fs::create_dir_all(&cold_root)
            .with_context(|| format!("create cold root {}", cold_root.display()))?;
        let store = object_store::local::LocalFileSystem::new_with_prefix(cold_root)
            .context("init local object store")?;
        Ok(Self {
            hot_root,
            cold_store: Arc::new(store),
            cold_bucket: bucket.into(),
        })
    }

    pub fn segment_data_path(&self, segment: SegmentId) -> PathBuf {
        self.hot_root
            .join("segments")
            .join(format!("{}.bin", segment.0))
    }

    /// Legacy stub path used by earlier prototypes (`<id>.stub.json`).
    pub fn segment_stub_path_legacy(&self, segment: SegmentId) -> PathBuf {
        self.hot_root
            .join("segments")
            .join(format!("{}.stub.json", segment.0))
    }

    pub fn cold_object_key(&self, segment: SegmentId) -> String {
        format!("segments/{}.bin", segment.0)
    }

    pub fn cold_object_path(&self, segment: SegmentId) -> ObjPath {
        ObjPath::from(self.cold_object_key(segment))
    }

    pub fn cold_remote_url(&self, segment: SegmentId) -> String {
        format!(
            "s3://{}/{}",
            self.cold_bucket,
            self.cold_object_key(segment)
        )
    }
}

pub async fn migrate_segment_to_cold(paths: &TieringPaths, segment: SegmentId) -> Result<()> {
    let data_path = paths.segment_data_path(segment);
    if !fs::try_exists(&data_path).await.unwrap_or(false) {
        return Err(anyhow!("segment data not found: {}", data_path.display()));
    }

    let raw = fs::read(&data_path)
        .await
        .with_context(|| format!("read segment {}", data_path.display()))?;
    if is_stub_bytes(&raw) {
        return Ok(());
    }

    let checksum = {
        let mut hasher = Sha256::new();
        hasher.update(&raw);
        format!("sha256:{}", hex::encode(hasher.finalize()))
    };

    let original_size = raw.len() as u64;
    let object_path = paths.cold_object_path(segment);
    paths
        .cold_store
        .put(&object_path, Bytes::from(raw).into())
        .await
        .with_context(|| format!("upload segment {} to cold store", segment.0))?;

    let stub = make_stub(paths.cold_remote_url(segment), original_size, checksum);
    let stub_bytes = serde_json::to_vec(&stub).context("serialize storage stub")?;

    write_atomic_replace(&data_path, &stub_bytes)
        .await
        .with_context(|| format!("replace {} with stub", data_path.display()))?;

    // Cleanup any legacy stub file from earlier prototypes.
    let _ = fs::remove_file(paths.segment_stub_path_legacy(segment)).await;

    Ok(())
}

pub async fn recall_segment_from_cold(
    paths: &TieringPaths,
    segment: SegmentId,
    reheat: bool,
) -> Result<Vec<u8>> {
    let data_path = paths.segment_data_path(segment);
    if fs::try_exists(&data_path).await.unwrap_or(false) {
        let raw = fs::read(&data_path)
            .await
            .with_context(|| format!("read segment {}", data_path.display()))?;
        if is_stub_bytes(&raw) {
            return recall_from_stub_bytes(paths, segment, &raw, reheat).await;
        }
        return Ok(raw);
    }

    // Legacy cold segments from earlier prototypes:
    // - missing `segments/<id>.bin`
    // - redirect stored in `segments/<id>.stub.json`
    let stub_path = paths.segment_stub_path_legacy(segment);
    if !fs::try_exists(&stub_path).await.unwrap_or(false) {
        return Err(anyhow!(
            "segment {} missing: no data file or stub",
            segment.0
        ));
    }

    let stub_bytes = fs::read(&stub_path)
        .await
        .with_context(|| format!("read stub {}", stub_path.display()))?;
    let legacy: serde_json::Value =
        serde_json::from_slice(&stub_bytes).context("deserialize legacy stub")?;
    let remote_key = legacy
        .get("remote_key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow!("legacy stub missing remote_key: {}", stub_path.display()))?;
    let object_path = ObjPath::from(remote_key);
    let get_result = paths
        .cold_store
        .get(&object_path)
        .await
        .with_context(|| format!("fetch legacy cold object {remote_key}"))?;
    let bytes = get_result.bytes().await?.to_vec();

    if reheat {
        if let Some(parent) = data_path.parent() {
            fs::create_dir_all(parent).await.ok();
        }
        write_atomic_replace(&data_path, &bytes)
            .await
            .with_context(|| format!("reheat segment {}", data_path.display()))?;
        let _ = fs::remove_file(&stub_path).await;
    }

    Ok(bytes)
}

pub async fn recall_from_stub_bytes(
    paths: &TieringPaths,
    segment: SegmentId,
    stub_bytes: &[u8],
    reheat: bool,
) -> Result<Vec<u8>> {
    let stub = parse_stub(stub_bytes)?;
    let object_path = object_path_from_remote_url(&stub.remote_url)?;
    let get_result = paths
        .cold_store
        .get(&object_path)
        .await
        .with_context(|| format!("fetch {} for segment {}", stub.remote_url, segment.0))?;
    let bytes = get_result
        .bytes()
        .await
        .with_context(|| format!("read body for segment {}", segment.0))?;

    if !stub.checksum.is_empty() {
        let mut hasher = Sha256::new();
        hasher.update(&bytes);
        let computed = format!("sha256:{}", hex::encode(hasher.finalize()));
        if computed != stub.checksum {
            return Err(anyhow!(
                "checksum mismatch for segment {}: expected {}, got {}",
                segment.0,
                stub.checksum,
                computed
            ));
        }
    }

    let out = bytes.to_vec();
    if reheat {
        let data_path = paths.segment_data_path(segment);
        if let Some(parent) = data_path.parent() {
            fs::create_dir_all(parent).await.ok();
        }
        write_atomic_replace(&data_path, &out)
            .await
            .with_context(|| format!("reheat segment {}", data_path.display()))?;
    }

    Ok(out)
}

pub async fn delete_segment_from_cold(paths: &TieringPaths, segment: SegmentId) -> Result<()> {
    let object_path = paths.cold_object_path(segment);
    let _ = paths.cold_store.delete(&object_path).await;
    Ok(())
}

async fn write_atomic_replace(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).await.ok();
    }
    let tmp = path.with_extension(format!(
        "{}.tmp",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    ));

    fs::write(&tmp, bytes)
        .await
        .with_context(|| format!("write {}", tmp.display()))?;
    let _ = fs::remove_file(path).await;
    fs::rename(&tmp, path)
        .await
        .with_context(|| format!("rename {} -> {}", tmp.display(), path.display()))?;
    Ok(())
}
