use anyhow::Result;
use async_trait::async_trait;
use common::{CapsuleId, Policy};
use std::any::Any;

#[async_trait]
pub trait PipelineStrategy: Send + Sync {
    /// Writes data to storage according to the provided policy.
    async fn write_capsule(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId>;

    /// Reads a full capsule back from storage.
    async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>>;

    /// Deletes a capsule and reclaims resources.
    async fn delete_capsule(&self, id: CapsuleId) -> Result<()>;

    /// Triggers garbage collection.
    async fn garbage_collect(&self) -> Result<usize>;

    /// Downcast support for strategy-specific configuration.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
