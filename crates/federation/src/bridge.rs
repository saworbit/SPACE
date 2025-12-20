use anyhow::{Context, Result};
use capsule_registry::CapsuleRegistry;
use common::{CapsuleId, FederationStrategy, Policy};
use nvram_sim::NvramLog;
use std::path::{Path, PathBuf};
use tracing::{info, warn};

/// Result summary for a federation application.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct FederationResult {
    pub zones_attempted: usize,
    pub zones_succeeded: usize,
}

/// Minimal Phase 4 federation bridge.
///
/// A "zone" is represented by a pair of files:
/// - `space.<zone>.db` (capsule metadata)
/// - `space.<zone>.nvram` (segment payload store)
#[derive(Debug, Clone)]
pub struct FederationBridge {
    base_dir: PathBuf,
}

impl FederationBridge {
    pub fn new(base_dir: impl Into<PathBuf>) -> Self {
        Self {
            base_dir: base_dir.into(),
        }
    }

    pub fn base_dir(&self) -> &Path {
        &self.base_dir
    }

    /// Apply `policy.federation` for `capsule_id` by replicating it into each target zone.
    pub async fn apply_policy(
        &self,
        capsule_id: CapsuleId,
        policy: &Policy,
        source_registry: &CapsuleRegistry,
        source_nvram: &NvramLog,
    ) -> Result<FederationResult> {
        let Some(federation) = policy.federation.as_ref() else {
            return Ok(FederationResult::default());
        };

        let mut result = FederationResult {
            zones_attempted: federation.target_zones.len(),
            zones_succeeded: 0,
        };

        for zone in &federation.target_zones {
            match self
                .replicate_capsule_to_zone(capsule_id, zone, source_registry, source_nvram)
                .await
            {
                Ok(()) => {
                    result.zones_succeeded += 1;
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        capsule = %capsule_id.as_uuid(),
                        zone,
                        "federation replication failed"
                    );
                }
            }
        }

        if matches!(federation.strategy, FederationStrategy::MoveTo) {
            // This repository doesn't yet implement reference-aware segment ownership,
            // so we do not delete local data. The move strategy is treated as
            // "replicate then handoff" for now.
            info!(
                capsule = %capsule_id.as_uuid(),
                "federation move-to requested (local cleanup not implemented)"
            );
        }

        Ok(result)
    }

    fn zone_registry_path(&self, zone: &str) -> PathBuf {
        let zone = sanitize_zone(zone);
        self.base_dir.join(format!("space.{zone}.db"))
    }

    fn zone_nvram_path(&self, zone: &str) -> PathBuf {
        let zone = sanitize_zone(zone);
        self.base_dir.join(format!("space.{zone}.nvram"))
    }

    /// Replicate a capsule (metadata + referenced segments) into a zone-scoped store.
    pub async fn replicate_capsule_to_zone(
        &self,
        capsule_id: CapsuleId,
        zone: &str,
        source_registry: &CapsuleRegistry,
        source_nvram: &NvramLog,
    ) -> Result<()> {
        let capsule = source_registry
            .lookup(capsule_id)
            .with_context(|| format!("lookup capsule {}", capsule_id.as_uuid()))?;

        let registry_path = self.zone_registry_path(zone);
        let nvram_path = self.zone_nvram_path(zone);

        let dest_registry = CapsuleRegistry::open(&registry_path)
            .with_context(|| format!("open zone registry {}", registry_path.display()))?;
        let dest_nvram = NvramLog::open(&nvram_path)
            .with_context(|| format!("open {}", nvram_path.display()))?;

        if dest_registry.lookup(capsule_id).is_ok() {
            info!(
                capsule = %capsule_id.as_uuid(),
                zone,
                "zone already has capsule metadata; skipping insert"
            );
            return Ok(());
        }

        let mut txn = dest_nvram
            .begin_transaction()
            .with_context(|| format!("begin nvram txn {}", nvram_path.display()))?;

        for seg_id in capsule.segments.iter().copied() {
            if dest_nvram.get_segment_metadata(seg_id).is_ok() {
                continue;
            }

            let payload = source_nvram
                .read(seg_id)
                .with_context(|| format!("read source segment {}", seg_id.0))?;
            let src_meta = source_nvram
                .get_segment_metadata(seg_id)
                .with_context(|| format!("read source segment metadata {}", seg_id.0))?;

            let appended = txn
                .append_segment(seg_id, &payload)
                .with_context(|| format!("append segment {}", seg_id.0))?;

            let mut dst_meta = src_meta;
            dst_meta.offset = appended.offset;
            dst_meta.len = appended.len;
            txn.set_segment_metadata(seg_id, dst_meta)
                .with_context(|| format!("set segment metadata {}", seg_id.0))?;
        }

        txn.commit()
            .with_context(|| format!("commit zone nvram {}", nvram_path.display()))?;

        dest_registry
            .create_capsule_with_segments(
                capsule.id,
                capsule.size,
                capsule.segments.clone(),
                capsule.policy.clone(),
            )
            .with_context(|| format!("insert capsule into zone registry {}", zone))?;

        info!(
            capsule = %capsule_id.as_uuid(),
            zone,
            segments = capsule.segments.len(),
            "federated capsule into zone"
        );

        Ok(())
    }
}

fn sanitize_zone(zone: &str) -> String {
    let mut out = String::with_capacity(zone.len());
    for ch in zone.chars() {
        if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "default".into()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsule_registry::pipeline::WritePipeline;
    use tempfile::TempDir;

    #[tokio::test]
    async fn replicates_capsule_into_zone_store() {
        let dir = TempDir::new().unwrap();
        let registry_path = dir.path().join("space.db");
        let nvram_path = dir.path().join("space.nvram");

        let registry = CapsuleRegistry::open(&registry_path).unwrap();
        let nvram = NvramLog::open(&nvram_path).unwrap();
        let pipeline = WritePipeline::new(registry.clone(), nvram.clone());

        let policy = Policy::default();
        let capsule_id = pipeline
            .write_capsule_with_policy(b"hello world", &policy)
            .await
            .unwrap();

        let mut policy_with_fed = policy.clone();
        policy_with_fed.federation = Some(common::FederationPolicy {
            strategy: FederationStrategy::ReplicateTo,
            target_zones: vec!["zone-b".into()],
        });

        let bridge = FederationBridge::new(dir.path());
        bridge
            .replicate_capsule_to_zone(capsule_id, "zone-b", &registry, &nvram)
            .await
            .unwrap();

        let zone_registry = CapsuleRegistry::open(dir.path().join("space.zone-b.db")).unwrap();
        let zone_nvram = NvramLog::open(dir.path().join("space.zone-b.nvram")).unwrap();
        let zone_pipeline = WritePipeline::new(zone_registry.clone(), zone_nvram);
        let bytes = zone_pipeline.read_capsule(capsule_id).await.unwrap();
        assert_eq!(bytes, b"hello world");
    }
}
