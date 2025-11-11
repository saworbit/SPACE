//! Lightweight Raft helper used to mock Paxos sharding for Phase 4 demos.
//! This crate provides the minimal API used by the scaling mesh until a full
//! Raft implementation is integrated.

use anyhow::Result;
use tracing::info;

/// Configuration used when creating a Raft cluster handle.
#[derive(Debug, Clone)]
pub struct RaftClusterConfig {
    pub name: String,
}

impl Default for RaftClusterConfig {
    fn default() -> Self {
        Self {
            name: "space-phase4".into(),
        }
    }
}

/// Represents the key used to store metadata shards.
#[derive(Debug, Clone)]
pub struct ShardKey(pub u64);

impl ShardKey {
    /// Create a new shard key from an identifier.
    pub fn new(id: u64) -> Self {
        Self(id)
    }
}

/// Lightweight Raft cluster stub used for metadata sharding in Phase 4.
#[derive(Debug)]
#[allow(dead_code)]
pub struct RaftCluster {
    config: RaftClusterConfig,
    zone_hint: Option<String>,
}

impl RaftCluster {
    /// Create a new RaftCluster handle from a config payload.
    pub fn new(config: RaftClusterConfig) -> Self {
        Self {
            config,
            zone_hint: None,
        }
    }

    /// Create a RaftCluster handle scoped to a specific zone.
    pub fn for_zone(zone: &str) -> Self {
        Self {
            config: RaftClusterConfig {
                name: format!("space-phase4-zone-{}", zone),
            },
            zone_hint: Some(zone.to_string()),
        }
    }

    /// Replicate a capsule identifier to another zone.
    pub async fn replicate(&self, capsule: &str, zone: &str) -> Result<()> {
        info!(capsule = %capsule, zone = %zone, "raft: replicating capsule metadata");
        Ok(())
    }

    /// Store a metadata shard payload in the Raft log.
    pub async fn store_shard(&self, shard: &ShardKey, payload: &[u8]) -> Result<()> {
        info!(
            shard = shard.0,
            bytes = payload.len(),
            "raft: storing metadata shard"
        );
        Ok(())
    }
}
