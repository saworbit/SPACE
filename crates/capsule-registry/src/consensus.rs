use serde::{Deserialize, Serialize};

use common::{Capsule, CapsuleId, ContentHash, SegmentId};
use std::sync::Arc;

use crate::store::MetadataStore;
use anyhow::Result;

/// Operations replicated through the Raft log.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MetadataOp {
    PutCapsule(Capsule),
    DeleteCapsule(CapsuleId),
    RegisterContent {
        hash: ContentHash,
        segment: SegmentId,
    },
    DeregisterContent {
        hash: ContentHash,
        segment: SegmentId,
    },
}

/// State machine responses surfaced to callers.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum OpResult {
    Ok,
    CapsuleFound(Capsule),
    NotFound,
    Error(String),
}

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
    fsm: MetadataStateMachine,
}

impl RaftNode {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self {
            fsm: MetadataStateMachine::new(store),
        }
    }

    /// Propose an operation; in single-node mode this applies immediately.
    #[allow(dead_code)]
    pub fn propose(&self, op: MetadataOp) -> Result<OpResult> {
        self.fsm.apply(op)
    }

    /// Produce a serialized snapshot for Raft snapshotting.
    #[allow(dead_code)]
    pub fn snapshot(&self) -> Result<Vec<u8>> {
        self.fsm.snapshot()
    }

    /// Restore state from a Raft snapshot payload.
    #[allow(dead_code)]
    pub fn restore(&self, data: &[u8]) -> Result<()> {
        self.fsm.restore_snapshot(data)
    }
}
