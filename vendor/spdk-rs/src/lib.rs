//! Simplified SPDK helper used by the Phase 4 NVMe view projection.

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
