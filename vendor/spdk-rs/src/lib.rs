//! SPDK bindings for SPACE.
//!
//! This crate provides two levels of SPDK integration:
//!
//! 1. **High-level abstractions** for Phase 4 capsule projection (mock implementations)
//! 2. **Low-level C FFI bindings** for Milestone 8.2 NVMe-oF integration (real SPDK)

use std::sync::Arc;

/// Low-level C FFI bindings to SPDK.
///
/// These bindings allow integration with the actual SPDK library for NVMe-oF
/// target functionality. They are used by the `protocol-nvme` crate's `foundry_bdev`
/// module.
pub mod bindings {
    use std::ffi::c_void;

    /// SPDK bdev I/O structure (opaque from Rust perspective).
    ///
    /// This is a forward declaration of the C struct. We don't need the full
    /// definition since we only work with pointers.
    #[repr(C)]
    pub struct spdk_bdev_io {
        /// Anonymous union containing I/O type-specific data.
        pub u: spdk_bdev_io_u,
    }

    /// Union containing I/O type-specific data.
    #[repr(C)]
    pub union spdk_bdev_io_u {
        pub bdev: std::mem::ManuallyDrop<spdk_bdev_io_u_bdev>,
    }

    /// Bdev-specific I/O data.
    #[repr(C)]
    #[derive(Copy, Clone)]
    pub struct spdk_bdev_io_u_bdev {
        /// Scatter-gather list for I/O.
        pub iovs: *mut iovec,
        /// Number of iovecs.
        pub iovcnt: i32,
        /// Starting block address.
        pub offset_blocks: u64,
        /// Number of blocks.
        pub num_blocks: u64,
    }

    /// POSIX iovec structure for scatter-gather I/O.
    #[repr(C)]
    pub struct iovec {
        /// Base address of memory region.
        pub iov_base: *mut c_void,
        /// Size of memory region.
        pub iov_len: usize,
    }

    /// SPDK poller structure (opaque).
    #[repr(C)]
    pub struct spdk_poller {
        _private: [u8; 0],
    }

    /// I/O completion status codes.
    pub const SPDK_BDEV_IO_STATUS_SUCCESS: i32 = 0;
    pub const SPDK_BDEV_IO_STATUS_FAILED: i32 = -1;
    pub const SPDK_BDEV_IO_STATUS_NOMEM: i32 = -2;

    /// Poller callback function type.
    ///
    /// Returns the number of events processed (for SPDK's busy/idle heuristics).
    pub type SpdkPollerFn = Option<extern "C" fn(arg: *mut c_void) -> i32>;

    // External C functions from SPDK library
    //
    // These are declared but not implemented here. Linking against libspdk
    // will provide the actual implementations.

    extern "C" {
        /// Register a poller to be called periodically.
        ///
        /// # Arguments
        ///
        /// * `fn` - Callback function
        /// * `arg` - Opaque argument passed to callback
        /// * `period_microseconds` - Polling period (0 = every tick)
        ///
        /// # Returns
        ///
        /// Pointer to poller handle, or NULL on error.
        pub fn spdk_poller_register(
            fn_: SpdkPollerFn,
            arg: *mut c_void,
            period_microseconds: u64,
        ) -> *mut spdk_poller;

        /// Complete a bdev I/O operation.
        ///
        /// This must be called exactly once for each I/O submitted to a bdev.
        ///
        /// # Safety
        ///
        /// - `bdev_io` must be a valid pointer from SPDK
        /// - Must be called from the SPDK thread
        /// - `bdev_io` must not be used after this call
        pub fn spdk_bdev_io_complete(bdev_io: *mut spdk_bdev_io, status: i32);
    }
}

// High-level abstractions for Phase 4 (mock implementations)

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
