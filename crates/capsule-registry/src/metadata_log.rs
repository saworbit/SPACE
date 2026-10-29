use std::sync::Arc;

use crate::metadata_ops::{MetadataOp, OpResult};
use crate::store::MetadataStore;
use anyhow::Result;

/// Applies metadata operations onto the local store.
#[derive(Clone)]
pub struct MetadataStateMachine {
    store: Arc<dyn MetadataStore>,
}

impl MetadataStateMachine {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self { store }
    }

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
}

/// Single-node metadata log. Every registry mutation goes through `apply`,
/// which runs it against the local store right away. Nothing is replicated.
#[derive(Clone)]
pub struct MetadataLog {
    fsm: MetadataStateMachine,
}

impl MetadataLog {
    pub fn new(store: Arc<dyn MetadataStore>) -> Self {
        Self {
            fsm: MetadataStateMachine::new(store),
        }
    }

    pub fn apply(&self, op: MetadataOp) -> Result<OpResult> {
        self.fsm.apply(op)
    }
}
