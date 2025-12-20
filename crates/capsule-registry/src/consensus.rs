use std::sync::Arc;

use crate::store::MetadataStore;
use crate::{metadata_ops::MetadataOp, metadata_ops::OpResult};
use anyhow::Result;

/// State machine that applies replicated metadata operations onto the storage engine.
#[derive(Clone)]
pub struct MetadataStateMachine {
    store: Arc<dyn MetadataStore>,
}

impl MetadataStateMachine {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self { store }
    }

    #[allow(dead_code)]
    pub fn apply(&self, op: MetadataOp) -> Result<OpResult> {
        match op {
            MetadataOp::PutCapsule(capsule) => {
                self.store.put_capsule(&capsule)?;
                Ok(OpResult::Ok)
            }
            MetadataOp::DeleteCapsule(id) => match self.store.delete_capsule(&id)? {
                Some(capsule) => Ok(OpResult::CapsuleFound(capsule)),
                None => Ok(OpResult::NotFound),
            },
            MetadataOp::RegisterContent { hash, segment } => {
                self.store.put_content(&hash, segment)?;
                Ok(OpResult::Ok)
            }
            MetadataOp::DeregisterContent { hash, segment } => {
                if let Some(current) = self.store.get_content(&hash)? {
                    if current == segment {
                        self.store.delete_content(&hash)?;
                        return Ok(OpResult::Ok);
                    }
                }
                Ok(OpResult::NotFound)
            }
        }
    }

    #[allow(dead_code)]
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.store.create_snapshot()
    }

    #[allow(dead_code)]
    pub fn restore_snapshot(&self, data: &[u8]) -> Result<()> {
        self.store.restore_snapshot(data)
    }
}

/// Thin Raft facade to route proposals into the state machine and expose snapshot hooks.
#[derive(Clone)]
pub struct RaftNode {
    inner: RaftInner,
}

#[derive(Clone)]
enum RaftInner {
    Single(MetadataStateMachine),
    #[allow(dead_code)]
    Distributed(crate::mesh::MeshRegistryRaft),
}

impl RaftNode {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self {
            inner: RaftInner::Single(MetadataStateMachine::new(store)),
        }
    }

    /// Propose an operation; in single-node mode this applies immediately.
    #[allow(dead_code)]
    pub fn propose(&self, op: MetadataOp) -> Result<OpResult> {
        match &self.inner {
            RaftInner::Single(fsm) => fsm.apply(op),
            RaftInner::Distributed(_) => anyhow::bail!("distributed raft requires async propose"),
        }
    }

    /// Propose an operation through Raft consensus (distributed or single).
    #[allow(dead_code)]
    pub async fn propose_async(&self, op: MetadataOp) -> Result<OpResult> {
        match &self.inner {
            RaftInner::Single(fsm) => fsm.apply(op),
            RaftInner::Distributed(mesh) => {
                let resp = mesh
                    .raft
                    .client_write(op)
                    .await
                    .map_err(|e| anyhow::anyhow!(e))?;
                Ok(resp.data)
            }
        }
    }

    /// Start a new single-node cluster (bootstraps membership and becomes leader).
    #[allow(dead_code)]
    pub async fn bootstrap_distributed(
        node_id: u64,
        raft_addr: std::net::SocketAddr,
        metadata_path: &str,
        raft_store_path: &str,
    ) -> Result<Self> {
        let mesh = crate::mesh::MeshRegistryRaft::start(
            node_id,
            raft_addr,
            metadata_path,
            raft_store_path,
            true,
        )
        .await?;
        Ok(Self {
            inner: RaftInner::Distributed(mesh),
        })
    }

    /// Start a node that will join an existing cluster after startup.
    #[allow(dead_code)]
    pub async fn join_distributed(
        node_id: u64,
        raft_addr: std::net::SocketAddr,
        leader_addr: std::net::SocketAddr,
        metadata_path: &str,
        raft_store_path: &str,
    ) -> Result<Self> {
        let mesh = crate::mesh::MeshRegistryRaft::start(
            node_id,
            raft_addr,
            metadata_path,
            raft_store_path,
            false,
        )
        .await?;

        crate::mesh::join_cluster(leader_addr, node_id, raft_addr).await?;

        Ok(Self {
            inner: RaftInner::Distributed(mesh),
        })
    }

    /// Best-effort: add a voter if this node is leader.
    #[allow(dead_code)]
    pub async fn add_voter(&self, node_id: u64, raft_addr: std::net::SocketAddr) -> Result<()> {
        let RaftInner::Distributed(mesh) = &self.inner else {
            return Ok(());
        };

        let node = openraft::BasicNode {
            addr: raft_addr.to_string(),
        };

        mesh.raft
            .add_learner(node_id, node, true)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        let metrics = mesh.raft.metrics();
        let current = metrics.borrow().clone();
        let mut voters: std::collections::BTreeSet<u64> =
            current.membership_config.voter_ids().collect();
        voters.insert(node_id);

        mesh.raft
            .change_membership(voters, true)
            .await
            .map_err(|e| anyhow::anyhow!(e))?;

        Ok(())
    }

    /// Produce a serialized snapshot for Raft snapshotting.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        match &self.inner {
            RaftInner::Single(fsm) => fsm.snapshot(),
            RaftInner::Distributed(_) => {
                anyhow::bail!("snapshot not supported in distributed mode")
            }
        }
    }

    /// Restore state from a Raft snapshot payload.
    #[allow(dead_code)]
    pub fn restore(&self, data: &[u8]) -> Result<()> {
        match &self.inner {
            RaftInner::Single(fsm) => fsm.restore_snapshot(data),
            RaftInner::Distributed(_) => anyhow::bail!("restore not supported in distributed mode"),
        }
    }
}
