//! Configuration for the PODMS orchestrator.

use common::Policy;
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// Configuration for the PODMS orchestrator.
///
/// This defines all the parameters needed to initialize and run a multi-node
/// SPACE deployment with autonomous scaling and replication.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct OrchestratorConfig {
    /// Node identifier (human-readable name)
    pub node_id: String,

    /// Listen address for incoming replication connections
    pub listen_addr: SocketAddr,

    /// Zone name for sovereignty and placement decisions
    pub zone_name: String,

    /// Default PODMS policy for capsules without explicit policies
    pub default_policy: Policy,

    /// Seed peer addresses for mesh discovery
    pub seed_peers: Vec<SocketAddr>,

    /// Gossip fanout (number of peers to gossip to each round)
    #[serde(default = "default_gossip_fanout")]
    pub gossip_fanout: usize,

    /// Gossip heartbeat interval in milliseconds
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval_ms: u64,

    /// Maximum gossip message TTL (hops)
    #[serde(default = "default_message_ttl")]
    pub message_ttl: u32,

    /// Maximum gossip message size in bytes
    #[serde(default = "default_max_message_size")]
    pub max_message_size: usize,

    /// Signing key for gossip message authentication (32 bytes)
    #[serde(default = "default_signing_key")]
    pub signing_key: Vec<u8>,
}

fn default_gossip_fanout() -> usize {
    8
}

fn default_heartbeat_interval() -> u64 {
    1000 // 1 second
}

fn default_message_ttl() -> u32 {
    10
}

fn default_max_message_size() -> usize {
    4096 // 4KB
}

fn default_signing_key() -> Vec<u8> {
    vec![0u8; 32] // Should be loaded from secure config
}

impl Default for OrchestratorConfig {
    fn default() -> Self {
        Self {
            node_id: "node-default".to_string(),
            listen_addr: "127.0.0.1:9000".parse().unwrap(),
            zone_name: "default-zone".to_string(),
            default_policy: Policy::metro_sync(),
            seed_peers: vec![],
            gossip_fanout: default_gossip_fanout(),
            heartbeat_interval_ms: default_heartbeat_interval(),
            message_ttl: default_message_ttl(),
            max_message_size: default_max_message_size(),
            signing_key: default_signing_key(),
        }
    }
}

impl OrchestratorConfig {
    /// Create a new orchestrator configuration with sensible defaults.
    ///
    /// # Arguments
    ///
    /// * `node_id` - Unique node identifier
    /// * `listen_addr` - Address to listen on for replication
    /// * `zone_name` - Zone for sovereignty constraints
    pub fn new(node_id: String, listen_addr: SocketAddr, zone_name: String) -> Self {
        Self {
            node_id,
            listen_addr,
            zone_name,
            ..Default::default()
        }
    }

    /// Add a seed peer address for mesh discovery.
    pub fn with_seed_peer(mut self, addr: SocketAddr) -> Self {
        self.seed_peers.push(addr);
        self
    }

    /// Set the default PODMS policy.
    pub fn with_policy(mut self, policy: Policy) -> Self {
        self.default_policy = policy;
        self
    }

    /// Set the gossip fanout.
    pub fn with_fanout(mut self, fanout: usize) -> Self {
        self.gossip_fanout = fanout;
        self
    }

    /// Set the gossip heartbeat interval.
    pub fn with_heartbeat_interval(mut self, interval_ms: u64) -> Self {
        self.heartbeat_interval_ms = interval_ms;
        self
    }

    /// Set the signing key for message authentication.
    ///
    /// # Security
    ///
    /// The signing key should be:
    /// - Exactly 32 bytes
    /// - Cryptographically random
    /// - Shared securely among trusted nodes
    /// - Never logged or exposed
    pub fn with_signing_key(mut self, key: Vec<u8>) -> Self {
        assert_eq!(key.len(), 32, "signing key must be exactly 32 bytes");
        self.signing_key = key;
        self
    }

    /// Load configuration from a YAML file.
    pub fn from_yaml_file(path: &str) -> anyhow::Result<Self> {
        let contents = std::fs::read_to_string(path)?;
        let config: OrchestratorConfig = serde_yaml::from_str(&contents)?;
        Ok(config)
    }

    /// Save configuration to a YAML file.
    pub fn to_yaml_file(&self, path: &str) -> anyhow::Result<()> {
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path, yaml)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = OrchestratorConfig::default();
        assert_eq!(config.gossip_fanout, 8);
        assert_eq!(config.heartbeat_interval_ms, 1000);
        assert_eq!(config.message_ttl, 10);
        assert_eq!(config.signing_key.len(), 32);
    }

    #[test]
    fn test_config_builder() {
        let config = OrchestratorConfig::new(
            "test-node".to_string(),
            "127.0.0.1:9001".parse().unwrap(),
            "test-zone".to_string(),
        )
        .with_seed_peer("127.0.0.1:9002".parse().unwrap())
        .with_fanout(16);

        assert_eq!(config.node_id, "test-node");
        assert_eq!(config.gossip_fanout, 16);
        assert_eq!(config.seed_peers.len(), 1);
    }

    #[test]
    #[should_panic(expected = "signing key must be exactly 32 bytes")]
    fn test_invalid_signing_key() {
        OrchestratorConfig::default().with_signing_key(vec![0u8; 16]); // Wrong length
    }
}
