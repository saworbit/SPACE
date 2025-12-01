use anyhow::Result;
use async_trait::async_trait;
use common::{CapsuleId, Policy};
use std::any::Any;

#[async_trait]
pub trait PipelineStrategy: Send + Sync + 'static {
    /// Writes data to storage according to the provided policy.
    async fn write_capsule(&self, data: &[u8], policy: &Policy) -> Result<CapsuleId>;

    /// Reads a full capsule back from storage.
    async fn read_capsule(&self, id: CapsuleId) -> Result<Vec<u8>>;

    /// Read a byte range from a capsule.
    ///
    /// Implementations should override this to perform efficient range reads.
    /// The default implementation performs a full read and slices the result to
    /// preserve backward compatibility.
    async fn read_range(&self, id: CapsuleId, offset: u64, len: usize) -> Result<Vec<u8>> {
        let full_data = self.read_capsule(id).await?;

        if offset >= full_data.len() as u64 {
            return Ok(Vec::new());
        }

        let end = std::cmp::min(offset + len as u64, full_data.len() as u64);
        Ok(full_data[offset as usize..end as usize].to_vec())
    }

    /// Deletes a capsule and reclaims resources.
    async fn delete_capsule(&self, id: CapsuleId) -> Result<()>;

    /// Triggers garbage collection.
    async fn garbage_collect(&self) -> Result<usize>;

    /// Downcast support for strategy-specific configuration.
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;
}
