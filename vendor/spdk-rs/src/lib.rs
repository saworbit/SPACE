//! Simplified SPDK helper used by the Phase 4 NVMe view projection.

use std::sync::Arc;

/// Represents an NVMe namespace that can be exported.
#[derive(Debug, Clone)]
pub struct Namespace {
    data: Vec<u8>,
}

impl Namespace {
    /// Create a new namespace with capsule data.
    pub fn new(data: Vec<u8>) -> Self {
        Self { data }
    }

    /// Access underlying blob for validation.
    pub fn as_slice(&self) -> &[u8] {
        &self.data
    }
}

/// Builder for NVMe targets.
#[derive(Debug, Default)]
pub struct NvmeTargetBuilder {
    namespaces: Vec<Namespace>,
}

impl NvmeTargetBuilder {
    /// Start a new builder.
    pub fn new() -> Self {
        Self {
            namespaces: Vec::new(),
        }
    }

    /// Add a namespace (capsule) to this target.
    pub fn add_namespace(&mut self, namespace: Namespace) -> &mut Self {
        self.namespaces.push(namespace);
        self
    }

    /// Finalize the NVMe target.
    pub fn build(self) -> NvmeTarget {
        NvmeTarget {
            namespaces: self.namespaces,
        }
    }
}

/// Handle referencing a projected NVMe target.
#[derive(Debug)]
pub struct NvmeTarget {
    namespaces: Vec<Namespace>,
}

impl NvmeTarget {
    /// Inspect namespaces attached to this target.
    pub fn namespaces(&self) -> &[Namespace] {
        &self.namespaces
    }
}

/// Logical block mapping returned by a virtual bdev callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockRange {
    /// Logical block address (offset in bytes for the mock).
    pub offset: u64,
    /// Length in bytes for this range.
    pub len: u64,
    /// Identifier for the backing segment/device.
    pub backing: String,
}

/// Virtual block device registered with SPDK.
#[derive(Clone)]
pub struct Bdev {
    name: String,
    size: u64,
    mapper: Arc<dyn Fn(u64, u64) -> Vec<BlockRange> + Send + Sync>,
}

impl std::fmt::Debug for Bdev {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Bdev")
            .field("name", &self.name)
            .field("size", &self.size)
            .finish()
    }
}

impl Bdev {
    /// Register a new virtual bdev that maps LBA ranges onto backing segments.
    pub fn register<F>(name: &str, size: u64, mapper: F) -> Result<Self, String>
    where
        F: Fn(u64, u64) -> Vec<BlockRange> + Send + Sync + 'static,
    {
        if name.is_empty() {
            return Err("bdev name cannot be empty".to_string());
        }
        Ok(Self {
            name: name.to_string(),
            size,
            mapper: Arc::new(mapper),
        })
    }

    /// Map a logical byte range to backing segments.
    pub fn map(&self, offset: u64, len: u64) -> Vec<BlockRange> {
        (self.mapper)(offset, len)
    }

    /// Name assigned to this bdev.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Logical size of the device in bytes.
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// NVMe-oF subsystem descriptor.
#[derive(Debug, Clone)]
pub struct NvmfSubsystem {
    nqn: String,
    bdev: String,
}

impl NvmfSubsystem {
    /// Create a subsystem exporting a registered bdev.
    pub fn create(nqn: &str, bdev: &str) -> Result<Self, String> {
        if nqn.is_empty() {
            return Err("nqn cannot be empty".into());
        }
        if bdev.is_empty() {
            return Err("bdev cannot be empty".into());
        }
        Ok(Self {
            nqn: nqn.to_string(),
            bdev: bdev.to_string(),
        })
    }

    /// Retrieve the exported NQN.
    pub fn nqn(&self) -> &str {
        &self.nqn
    }

    /// Underlying bdev name.
    pub fn bdev(&self) -> &str {
        &self.bdev
    }
}
