use std::sync::Arc;

use anyhow::Result;
use common::traits::CapsuleCatalog;
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
    catalog: Arc<dyn CapsuleCatalog>,
    config: TieringConfig,
    heatmap: Heatmap,
) -> Result<TieringAgentHandle> {
    let paths = TieringPaths {
        hot_root: config.hot_root.clone(),
        cold_root: config.cold_root.clone(),
    };

    let handle = tokio::spawn(async move {
        loop {
            let candidates =
                heatmap.cold_candidates(config.cold_threshold, config.max_capsules_per_scan);
            for capsule_id in candidates {
                let capsule = match catalog.lookup_capsule(capsule_id) {
                    Ok(c) => c,
                    Err(err) => {
                        tracing::debug!(error = %err, capsule = %capsule_id.as_uuid(), "tiering: lookup failed");
                        continue;
                    }
                };

                for segment in capsule.segments {
                    if let Err(err) = migrate_segment_to_cold(&paths, segment).await {
                        tracing::debug!(
                            error = %err,
                            capsule = %capsule_id.as_uuid(),
                            segment = segment.0,
                            "tiering: migrate failed"
                        );
                    }
                }
            }

            tokio::time::sleep(config.scan_interval).await;
        }
    });

    Ok(TieringAgentHandle { handle })
}
