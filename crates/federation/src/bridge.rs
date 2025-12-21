use crate::queue::{ReplicationJob, ReplicationQueue};
use crate::state::ReplicationState;
use crate::wan::{PeerClientManager, WanTransferAgent};
use crate::zones::ZoneDirectory;
use anyhow::{Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{Capsule, CapsuleId};
use nvram_sim::NvramLog;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::time::{sleep, Duration};
use tracing::{debug, info, warn};

/// Result summary for a federation application.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FederationResult {
    pub zones_attempted: usize,
    pub zones_succeeded: usize,
}

/// Phase 4b federation bridge ("Customs Officer").
///
/// In production this would subscribe to registry events; in this repository
/// we support explicit invocation and an optional polling loop.
#[derive(Clone)]
pub struct Bridge {
    local_registry: Arc<CapsuleRegistry>,
    local_nvram: Arc<NvramLog>,
    zones: Arc<ZoneDirectory>,
    peer_clients: Arc<PeerClientManager>,
    transfer: WanTransferAgent,
    queue: ReplicationQueue,
    state: ReplicationState,
}

/// Backwards/compat name matching the Phase 4b docs ("FederationBridge").
pub type FederationBridge = Bridge;

impl Bridge {
    pub fn open(
        local_registry: Arc<CapsuleRegistry>,
        local_nvram: Arc<NvramLog>,
        zones: ZoneDirectory,
        local_zone_id: impl Into<String>,
        queue_path: &Path,
        state_path: &Path,
    ) -> Result<Self> {
        Ok(Self {
            local_registry,
            local_nvram,
            zones: Arc::new(zones),
            peer_clients: Arc::new(PeerClientManager::new(local_zone_id)),
            transfer: WanTransferAgent::default(),
            queue: ReplicationQueue::open(queue_path)?,
            state: ReplicationState::open(state_path)?,
        })
    }

    pub fn open_default(
        local_registry: Arc<CapsuleRegistry>,
        local_nvram: Arc<NvramLog>,
        local_zone_id: impl Into<String>,
    ) -> Result<Self> {
        let zones_path = crate::zones::ZoneDirectory::default_path()?;
        let zones = ZoneDirectory::load(&zones_path)?;

        let dir = zones_path
            .parent()
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."));
        std::fs::create_dir_all(&dir)
            .with_context(|| format!("create federation state dir {}", dir.display()))?;

        let queue_path = dir.join("federation.queue.db");
        let state_path = dir.join("federation.state.db");

        Self::open(
            local_registry,
            local_nvram,
            zones,
            local_zone_id,
            &queue_path,
            &state_path,
        )
    }

    pub async fn apply_policy(&self, capsule_id: CapsuleId) -> Result<FederationResult> {
        let capsule = self
            .local_registry
            .lookup(capsule_id)
            .with_context(|| format!("lookup capsule {}", capsule_id.as_uuid()))?;
        self.enqueue_capsule(&capsule)?;
        self.drain_queue().await
    }

    pub fn enqueue_capsule(&self, capsule: &Capsule) -> Result<FederationResult> {
        let targets = capsule
            .policy
            .federation
            .targets
            .iter()
            .map(|t| t.trim())
            .filter(|t| !t.is_empty());

        let mut attempted = 0usize;
        let mut scheduled = 0usize;

        for target in targets {
            attempted += 1;

            if self.state.is_synced(capsule.id, target)? {
                debug!(
                    capsule = %capsule.id.as_uuid(),
                    zone = target,
                    "federation already synced; skipping"
                );
                continue;
            }

            if self.zones.get(target).is_none() {
                warn!(
                    capsule = %capsule.id.as_uuid(),
                    zone = target,
                    "unknown federation zone; add it via spacectl zone add"
                );
                continue;
            }

            let job = ReplicationJob {
                capsule_id: capsule.id,
                target_zone: target.to_string(),
                priority: capsule.policy.federation.priority.clone(),
            };
            if self.queue.enqueue(&job)? {
                scheduled += 1;
            }
        }

        Ok(FederationResult {
            zones_attempted: attempted,
            zones_succeeded: scheduled,
        })
    }

    pub async fn drain_queue(&self) -> Result<FederationResult> {
        let mut attempted = 0usize;
        let mut succeeded = 0usize;

        while let Some(job) = self.queue.dequeue_next()? {
            attempted += 1;
            match self.process_job(&job).await {
                Ok(()) => {
                    succeeded += 1;
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        capsule = %job.capsule_id.as_uuid(),
                        zone = %job.target_zone,
                        "federation job failed"
                    );
                }
            }
        }

        Ok(FederationResult {
            zones_attempted: attempted,
            zones_succeeded: succeeded,
        })
    }

    async fn process_job(&self, job: &ReplicationJob) -> Result<()> {
        let zone = self
            .zones
            .get(&job.target_zone)
            .with_context(|| format!("unknown zone {}", job.target_zone))?;

        if self.state.is_synced(job.capsule_id, &job.target_zone)? {
            return Ok(());
        }

        self.transfer
            .replicate_capsule(
                job.capsule_id,
                self.local_registry.as_ref(),
                self.local_nvram.as_ref(),
                &self.peer_clients,
                zone,
                job.priority.clone(),
            )
            .await?;

        self.state.mark_synced(job.capsule_id, &job.target_zone)?;
        Ok(())
    }

    pub async fn run_polling(&self, interval: Duration) -> Result<()> {
        loop {
            self.scan_for_new_capsules()?;
            let _ = self.drain_queue().await?;
            sleep(interval).await;
        }
    }

    fn scan_for_new_capsules(&self) -> Result<()> {
        let mut cursor = None;
        loop {
            let ids = self
                .local_registry
                .list_capsules(1024, cursor)
                .context("list_capsules")?;
            if ids.is_empty() {
                break;
            }
            cursor = ids.last().copied();
            for id in ids {
                if let Ok(capsule) = self.local_registry.lookup(id) {
                    let _ = self.enqueue_capsule(&capsule)?;
                }
            }
        }
        info!("federation scan complete");
        Ok(())
    }
}
