use std::sync::Arc;

use anyhow::Result;
use tokio::task::JoinHandle;

use crate::{migrate_segment_to_cold, Heatmap, TieringConfig, TieringPaths};

#[derive(Clone)]
pub struct TieringAgent {
    paths: Arc<TieringPaths>,
    config: TieringConfig,
    heatmap: Heatmap,
}

impl TieringAgent {
    pub fn new(paths: Arc<TieringPaths>, config: TieringConfig, heatmap: Heatmap) -> Self {
        Self {
            paths,
            config,
            heatmap,
        }
    }

    pub async fn run(self) {
        loop {
            let candidates = self.heatmap.cold_candidates(
                self.config.cold_threshold,
                self.config.max_segments_per_scan,
            );

            for segment in candidates {
                if let Err(err) = migrate_segment_to_cold(&self.paths, segment).await {
                    tracing::debug!(
                        error = %err,
                        segment = segment.0,
                        "tiering: migrate failed"
                    );
                }
            }

            tokio::time::sleep(self.config.scan_interval).await;
        }
    }
}

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
    let agent = TieringAgent::new(paths, config, heatmap);
    let handle = tokio::spawn(agent.run());

    Ok(TieringAgentHandle { handle })
}
