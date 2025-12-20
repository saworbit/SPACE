//! Phase 4 "View" projection for local file semantics.
//!
//! This crate intentionally avoids tying the engine to a specific kernel FUSE
//! implementation. Instead it provides a small adapter that can "materialize"
//! a capsule into a directory by exposing a `content` file. On Unix platforms we
//! prefer a FIFO so the mount appears quickly and reads stream from the pipeline.
#![cfg(feature = "phase4")]

#[cfg(unix)]
use anyhow::anyhow;
use anyhow::{Context, Result};
use capsule_registry::pipeline::WritePipeline;
use capsule_registry::CapsuleRegistry;
use common::CapsuleId;
use common::Policy;
use scaling::enforce_view_policy;
use scaling::MeshNode;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::AsyncWriteExt;
use tokio::sync::watch;
use tracing::{info, info_span, warn};

const CONTENT_FILENAME: &str = "content";
const METADATA_FILENAME: &str = "space.json";
const DEFAULT_STREAM_CHUNK: usize = 1024 * 1024; // 1 MiB

/// Handle representing a projected capsule view.
#[derive(Debug)]
pub struct SpaceViewMount {
    mountpoint: PathBuf,
    shutdown: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl SpaceViewMount {
    pub fn mountpoint(&self) -> &Path {
        &self.mountpoint
    }

    pub async fn unmount(self) -> Result<()> {
        let _ = self.shutdown.send(true);
        let _ = self.task.await;
        Ok(())
    }
}

#[cfg(unix)]
fn ensure_fifo(path: &Path) -> Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::FileTypeExt;

    if path.exists() {
        // If something already exists at the target path, require that it is a FIFO.
        let meta = std::fs::symlink_metadata(path)?;
        if meta.file_type().is_fifo() {
            return Ok(());
        }
        return Err(anyhow!("path exists but is not a fifo: {}", path.display()));
    }

    let c_path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| anyhow!("fifo path contains interior NUL"))?;
    let rc = unsafe { libc::mkfifo(c_path.as_ptr(), 0o644) };
    if rc != 0 {
        return Err(std::io::Error::last_os_error())
            .with_context(|| format!("mkfifo {}", path.display()));
    }
    Ok(())
}

async fn write_metadata(mountpoint: &Path, capsule_id: CapsuleId, policy: &Policy) -> Result<()> {
    let meta_path = mountpoint.join(METADATA_FILENAME);
    let payload = serde_json::json!({
        "capsule_id": capsule_id.as_uuid().to_string(),
        "policy": policy,
    });
    tokio::fs::write(&meta_path, serde_json::to_vec_pretty(&payload)?)
        .await
        .with_context(|| format!("write {}", meta_path.display()))?;
    Ok(())
}

#[cfg(unix)]
async fn open_fifo_for_write(
    path: &Path,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Option<tokio::fs::File> {
    use tokio::time::{sleep, Duration};

    loop {
        if *shutdown_rx.borrow() {
            return None;
        }

        match tokio::fs::OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)
            .await
        {
            Ok(file) => return Some(file),
            Err(err) => {
                // `ENXIO` is expected when there is no reader yet.
                if err.raw_os_error() != Some(libc::ENXIO) {
                    warn!(error = %err, path = %path.display(), "failed to open fifo for write");
                }

                tokio::select! {
                    _ = shutdown_rx.changed() => {},
                    _ = sleep(Duration::from_millis(50)) => {},
                }
            }
        }
    }
}

/// Mount a capsule as a local filesystem-style view.
///
/// The mountpoint will contain:
/// - `content`: a streamable file representing the capsule bytes
/// - `space.json`: metadata describing the capsule/policy used for enforcement
pub async fn mount_fuse_view<C: scaling::ContentStore + 'static>(
    capsule_id: CapsuleId,
    policy: &Policy,
    mesh: &MeshNode<C>,
    pipeline: Arc<WritePipeline>,
    mountpoint: impl AsRef<Path>,
    registry: &CapsuleRegistry,
) -> Result<SpaceViewMount> {
    let mountpoint = mountpoint.as_ref().to_path_buf();
    let mountpoint_display = mountpoint.display().to_string();
    let span = info_span!(
        "view_mount",
        capsule = %capsule_id.as_uuid(),
        mountpoint = %mountpoint_display
    );
    let _enter = span.enter();

    let capsule = registry
        .lookup(capsule_id)
        .with_context(|| format!("lookup capsule {}", capsule_id.as_uuid()))?;

    enforce_view_policy(mesh, capsule_id, policy, "fuse", |cid| {
        registry.serialize_capsule(cid)
    })
    .await?;

    tokio::fs::create_dir_all(&mountpoint)
        .await
        .with_context(|| format!("create dir {}", mountpoint.display()))?;
    write_metadata(&mountpoint, capsule_id, policy).await?;

    let content_path = mountpoint.join(CONTENT_FILENAME);

    #[cfg(unix)]
    ensure_fifo(&content_path)?;

    let size = capsule.size;
    let (shutdown, mut shutdown_rx) = watch::channel(false);

    let task = tokio::spawn(async move {
        loop {
            if *shutdown_rx.borrow() {
                break;
            }

            #[cfg(unix)]
            let mut writer = match open_fifo_for_write(&content_path, &mut shutdown_rx).await {
                Some(file) => file,
                None => break,
            };

            #[cfg(not(unix))]
            let mut writer = match tokio::fs::OpenOptions::new()
                .create(true)
                .write(true)
                .truncate(true)
                .open(&content_path)
                .await
            {
                Ok(file) => file,
                Err(err) => {
                    warn!(
                        error = %err,
                        path = %content_path.display(),
                        "failed to open view content"
                    );
                    let _ = shutdown_rx.changed().await;
                    continue;
                }
            };

            let mut offset: u64 = 0;
            while offset < size {
                if *shutdown_rx.borrow() {
                    break;
                }

                let remaining = (size - offset) as usize;
                let chunk_len = std::cmp::min(DEFAULT_STREAM_CHUNK, remaining);
                let chunk = match pipeline.read_range(capsule_id, offset, chunk_len).await {
                    Ok(bytes) => bytes,
                    Err(err) => {
                        warn!(error = %err, offset, len = chunk_len, "failed to read capsule range for view");
                        break;
                    }
                };

                if chunk.is_empty() {
                    break;
                }

                if let Err(err) = writer.write_all(&chunk).await {
                    warn!(error = %err, "failed to write view content");
                    break;
                }
                offset = offset.saturating_add(chunk.len() as u64);
            }

            let _ = writer.flush().await;

            // For regular files (non-unix), one projection is enough.
            #[cfg(not(unix))]
            break;

            // For FIFOs, loop back to handle the next reader.
            #[cfg(unix)]
            if shutdown_rx.changed().await.is_err() {
                break;
            }
        }
    });

    info!(
        capsule = %capsule_id.as_uuid(),
        mountpoint = %mountpoint_display,
        "mounted view (content file)"
    );

    Ok(SpaceViewMount {
        mountpoint,
        shutdown,
        task,
    })
}

#[cfg(all(test, feature = "phase4"))]
mod tests {
    use super::*;
    use capsule_registry::pipeline::WritePipeline;
    use capsule_registry::CapsuleRegistry;
    use common::podms::ZoneId;
    use common::{CapsuleId, ContentHash, Policy, SegmentId};
    use encryption::KeyManager;
    use nvram_sim::NvramLog;
    use scaling::{ContentStore, MeshNode};
    use std::net::{IpAddr, Ipv4Addr, SocketAddr};
    use std::sync::Arc;
    use tempfile::TempDir;
    use tokio::sync::RwLock;

    #[derive(Default)]
    struct DummyContentStore;

    impl ContentStore for DummyContentStore {
        fn lookup_content(&self, _hash: &ContentHash) -> Option<SegmentId> {
            None
        }

        fn register_content(&self, _hash: &ContentHash, _segment_id: SegmentId) {}
    }

    async fn build_mesh(zone: ZoneId) -> MeshNode<DummyContentStore> {
        let content = Arc::new(RwLock::new(DummyContentStore));
        let nvram_path =
            std::env::temp_dir().join(format!("fuse-mesh-{}.log", CapsuleId::new().as_uuid()));
        let nvram = Arc::new(RwLock::new(
            NvramLog::open(nvram_path.to_string_lossy().as_ref()).expect("open nvram"),
        ));
        let key_manager = Arc::new(RwLock::new(KeyManager::new([0u8; 32])));
        MeshNode::new(
            zone,
            SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 0),
            content,
            nvram,
            key_manager,
        )
        .await
        .unwrap()
    }

    fn temp_mount() -> (TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let mountpoint = dir.path().join("mnt");
        (dir, mountpoint)
    }

    #[tokio::test]
    async fn view_mount_creates_content_path() {
        let registry = CapsuleRegistry::new();
        let capsule_id = CapsuleId::new();
        let policy = Policy::metro_sync();
        registry
            .create_capsule_with_segments(capsule_id, 0, Vec::new(), policy.clone())
            .unwrap();

        let mesh = build_mesh(ZoneId::Metro {
            name: "fuse-test".into(),
        })
        .await;

        let nvram = NvramLog::open(std::env::temp_dir().join("fuse-view.nvram")).unwrap();
        let pipeline = Arc::new(WritePipeline::new(registry.clone(), nvram));
        let (_dir, mountpoint) = temp_mount();

        let handle = mount_fuse_view(capsule_id, &policy, &mesh, pipeline, &mountpoint, &registry)
            .await
            .unwrap();

        let content_path = handle.mountpoint().join(CONTENT_FILENAME);
        let mut attempts = 0;
        while !content_path.exists() && attempts < 50 {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            attempts += 1;
        }
        assert!(content_path.exists(), "content path should exist");
    }
}
