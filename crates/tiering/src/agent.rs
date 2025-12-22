use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinHandle;

use crate::{migrate_segment_to_cold, Heatmap, TieringConfig, TieringPaths};

pub struct TieringAgentHandle {
    handle: JoinHandle<()>,
}

impl TieringAgentHandle {
    pub async fn stop(self) {
        self.handle.abort();
        let _ = self.handle.await;
    }
}

pub fn spawn_tiering_agent(
    paths: Arc<TieringPaths>,
    config: TieringConfig,
    heatmap: Heatmap,
) -> Result<TieringAgentHandle> {
    let handle = tokio::spawn(async move {
        loop {
            let candidates =
                heatmap.cold_candidates(config.cold_threshold, config.max_segments_per_scan);

            for segment in candidates {
                if let Err(err) = migrate_segment_to_cold(&paths, segment).await {
                    tracing::debug!(
                        error = %err,
                        segment = segment.0,
                        "tiering: migrate failed"
                    );
                }
            }

            tokio::time::sleep(config.scan_interval).await;
        }
    });

    Ok(TieringAgentHandle { handle })
}
