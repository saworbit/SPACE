use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tokio::task::JoinHandle;

use crate::Heatmap;

#[cfg(feature = "audit")]
use common::security::audit_log::AuditRecord;

#[cfg(feature = "audit")]
use common::Event;

pub struct AuditWatcherHandle {
    handle: JoinHandle<()>,
}

impl AuditWatcherHandle {
    pub async fn stop(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

pub fn spawn_audit_heatmap_watcher(
    heatmap: Heatmap,
    audit_log_path: PathBuf,
    poll_interval: Duration,
) -> Result<AuditWatcherHandle> {
    let handle = tokio::spawn(async move {
        let mut offset: u64 = 0;
        loop {
            if let Ok(metadata) = tokio::fs::metadata(&audit_log_path).await {
                let len = metadata.len();
                if len < offset {
                    offset = 0;
                }
                if len > offset {
                    if let Ok(chunk) = read_range(&audit_log_path, offset, len - offset).await {
                        offset = len;
                        for line in chunk.split_terminator('\n') {
                            if line.trim().is_empty() {
                                continue;
                            }
                            match serde_json::from_str::<AuditRecord>(line) {
                                Ok(record) => match record.event {
                                    Event::CapsuleRead { capsule_id, .. } => {
                                        heatmap.touch(capsule_id);
                                    }
                                    Event::CapsuleCreated { capsule_id, .. } => {
                                        heatmap.touch(capsule_id);
                                    }
                                    _ => {}
                                },
                                Err(err) => {
                                    tracing::debug!(error = %err, "audit watcher: parse failure");
                                }
                            }
                        }
                    }
                }
            }

            tokio::time::sleep(poll_interval).await;
        }
    });

    Ok(AuditWatcherHandle { handle })
}

async fn read_range(path: &PathBuf, offset: u64, len: u64) -> Result<String> {
    use tokio::io::{AsyncReadExt, AsyncSeekExt};
    let mut file = tokio::fs::File::open(path).await?;
    file.seek(std::io::SeekFrom::Start(offset)).await?;
    let mut buf = vec![0u8; len as usize];
    file.read_exact(&mut buf).await?;
    Ok(String::from_utf8_lossy(&buf).to_string())
}
