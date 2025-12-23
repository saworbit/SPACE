//! SPDK bdev module implementation for Foundry volumes.
//!
//! This module provides the C ABI interface that SPDK expects for a block device.
//! It handles the translation between SPDK's synchronous polling model and our
//! async bridge to the Foundry backend.
//!
//! ## Safety
//!
//! This module contains extensive unsafe code due to FFI with SPDK's C API.
//! Key invariants:
//!
//! - All SPDK functions must only be called from the SPDK thread
//! - `spdk_bdev_io` pointers must remain valid until `spdk_bdev_io_complete` is called
//! - The global `BRIDGE` is initialized once and never mutated afterward
//!
//! ## Block Size
//!
//! We assume 4096-byte blocks (standard NVMe block size). SPDK provides block
//! addresses which we multiply by 4096 to get byte offsets.

use std::ffi::c_void;
use std::sync::Arc;

use crate::foundry_bdev::bridge::{IoBridge, NvmeCommand, NvmeCompletion};
use foundry::VolumeBackend;

// Re-export bindings from spdk-rs
// Note: These will need to be implemented in vendor/spdk-rs
use spdk_rs::bindings::*;

/// Standard NVMe block size (4 KiB).
const BLOCK_SIZE: u64 = 4096;

/// Global bridge instance (initialized once during bdev initialization).
///
/// # Safety
///
/// This is a static mut, which is generally unsafe. However, SPDK's architecture
/// guarantees single-threaded access from the reactor thread, so this is safe
/// in practice. Initialization happens once during `init_foundry_bdev`, and
/// afterward only the SPDK thread accesses it.
#[allow(static_mut_refs)]
static mut BRIDGE: Option<IoBridge> = None;

/// Initialize the Foundry bdev context.
///
/// This must be called once before any I/O operations. It:
///
/// 1. Creates the async I/O bridge
/// 2. Registers the SPDK poller
/// 3. (Future) Registers the bdev with SPDK
///
/// # Arguments
///
/// * `volume` - The Foundry volume to expose via NVMe-oF
///
/// # Safety
///
/// This function must be called from the SPDK reactor thread. It initializes
/// global state that will be accessed by SPDK callbacks.
pub unsafe fn init_foundry_bdev(volume: Arc<dyn VolumeBackend>) {
    tracing::info!("Initializing Foundry bdev");

    // Create the async bridge
    BRIDGE = Some(IoBridge::new(volume));

    // Register poller with SPDK
    // The poller will be called on every reactor tick (typically microseconds)
    let poller = spdk_poller_register(
        Some(foundry_bdev_poll),
        std::ptr::null_mut(),
        0, // period_microseconds = 0 means "call every tick"
    );

    if poller.is_null() {
        tracing::error!("Failed to register SPDK poller");
        return;
    }

    tracing::info!("Foundry bdev initialized, poller registered");
}

/// The SPDK poller callback.
///
/// This is called on every tick of the SPDK reactor (typically thousands of times
/// per second). It checks the completion queue from the Tokio bridge and completes
/// any finished I/O operations.
///
/// # Returns
///
/// The number of completions processed (used by SPDK for busy/idle heuristics).
///
/// # Safety
///
/// This function is called from the SPDK reactor thread. It must not block and
/// must complete quickly to avoid stalling the reactor.
#[no_mangle]
#[allow(static_mut_refs)]
extern "C" fn foundry_bdev_poll(_arg: *mut c_void) -> i32 {
    let bridge = unsafe {
        match BRIDGE.as_ref() {
            Some(b) => b,
            None => {
                tracing::error!("Poller called before bridge initialization");
                return 0;
            }
        }
    };

    let mut count = 0;

    // Process all available completions
    while let Some(completion) = bridge.poll_completions() {
        count += 1;

        match completion {
            NvmeCompletion::Success { io_ptr, data } => {
                unsafe {
                    let io = io_ptr as *mut spdk_bdev_io;

                    // If this is a read, copy data into SPDK's iovec
                    if let Some(read_bytes) = data {
                        copy_to_iovec(io, &read_bytes);
                    }

                    // Complete the I/O with success status
                    spdk_bdev_io_complete(io, SPDK_BDEV_IO_STATUS_SUCCESS);
                }
            }
            NvmeCompletion::Error { io_ptr, status } => unsafe {
                let io = io_ptr as *mut spdk_bdev_io;
                tracing::warn!(status, "Completing I/O with error");
                spdk_bdev_io_complete(io, SPDK_BDEV_IO_STATUS_FAILED);
            },
        }
    }

    count
}

/// Copy read data into SPDK's iovec structure.
///
/// SPDK provides a scatter-gather list (iovec array) for I/O. For simplicity,
/// we currently assume a single contiguous buffer. A production implementation
/// should iterate over all iovecs.
///
/// # Safety
///
/// - `io` must be a valid `spdk_bdev_io` pointer
/// - The iovec must be large enough to hold `data`
unsafe fn copy_to_iovec(io: *mut spdk_bdev_io, data: &[u8]) {
    let bdev = &*(*io).u.bdev;
    let iovs = bdev.iovs;
    let iovcnt = bdev.iovcnt;

    if iovcnt == 0 {
        tracing::error!("No iovecs available for read data");
        return;
    }

    // For now, assume single iovec (common case)
    let iov = &*iovs;
    let dst = iov.iov_base as *mut u8;
    let dst_len = iov.iov_len;

    if data.len() > dst_len {
        tracing::error!(
            data_len = data.len(),
            iov_len = dst_len,
            "Read data exceeds iovec size"
        );
        return;
    }

    // Copy data from Foundry (Bytes) to SPDK (iovec)
    std::ptr::copy_nonoverlapping(data.as_ptr(), dst, data.len());
}

/// Submit a read or write I/O to the bridge.
///
/// This is called from SPDK's bdev I/O submission path. It translates the
/// SPDK I/O request into our async command format and submits it to the bridge.
///
/// # Arguments
///
/// * `io` - SPDK I/O descriptor
/// * `is_write` - true for writes, false for reads
///
/// # Safety
///
/// - `io` must be a valid `spdk_bdev_io` pointer
/// - `io` must remain valid until `spdk_bdev_io_complete` is called
#[allow(static_mut_refs)]
pub unsafe fn submit_io(io: *mut spdk_bdev_io, is_write: bool) {
    let bridge = match BRIDGE.as_ref() {
        Some(b) => b,
        None => {
            tracing::error!("submit_io called before bridge initialization");
            spdk_bdev_io_complete(io, SPDK_BDEV_IO_STATUS_FAILED);
            return;
        }
    };

    // Extract block address and count from SPDK I/O
    let bdev = &*(*io).u.bdev;
    let offset_blocks = bdev.offset_blocks;
    let num_blocks = bdev.num_blocks;

    // Convert to byte offsets
    let offset = offset_blocks * BLOCK_SIZE;
    let len = num_blocks * BLOCK_SIZE;

    tracing::trace!(offset, len, is_write, "Submitting I/O to bridge");

    let io_ptr = io as *mut c_void;

    if is_write {
        // Copy data from SPDK iovec to Bytes
        let data = match copy_from_iovec(io, len as usize) {
            Some(d) => d,
            None => {
                spdk_bdev_io_complete(io, SPDK_BDEV_IO_STATUS_FAILED);
                return;
            }
        };

        bridge.submit(NvmeCommand::Write {
            offset,
            data,
            io_ptr,
        });
    } else {
        bridge.submit(NvmeCommand::Read {
            offset,
            len,
            io_ptr,
        });
    }
}

/// Copy write data from SPDK's iovec to a Bytes buffer.
///
/// # Safety
///
/// - `io` must be a valid `spdk_bdev_io` pointer
unsafe fn copy_from_iovec(io: *mut spdk_bdev_io, len: usize) -> Option<bytes::Bytes> {
    let bdev = &*(*io).u.bdev;
    let iovs = bdev.iovs;
    let iovcnt = bdev.iovcnt;

    if iovcnt == 0 {
        tracing::error!("No iovecs available for write data");
        return None;
    }

    // For now, assume single iovec (common case)
    let iov = &*iovs;
    let src = iov.iov_base as *const u8;

    if len > iov.iov_len {
        tracing::error!(
            requested_len = len,
            iov_len = iov.iov_len,
            "Write length exceeds iovec size"
        );
        return None;
    }

    // Copy data from SPDK to owned Bytes
    let slice = std::slice::from_raw_parts(src, len);
    Some(bytes::Bytes::copy_from_slice(slice))
}

/// Shutdown the Foundry bdev.
///
/// This is called when the SPDK target is shutting down. It cleans up the
/// bridge and unregisters resources.
///
/// # Safety
///
/// Must be called from the SPDK reactor thread.
pub unsafe fn shutdown_foundry_bdev() {
    tracing::info!("Shutting down Foundry bdev");
    BRIDGE = None;
}
