//! The async bridge between SPDK's polling reactor and Tokio's async runtime.
//!
//! ## Architecture
//!
//! SPDK runs on a pinned CPU core with a polling event loop. Foundry runs on
//! Tokio's work-stealing thread pool. We cannot block the SPDK thread waiting
//! for async I/O, so we use a channel-based architecture:
//!
//! 1. SPDK thread submits commands via MPSC channel
//! 2. Tokio worker receives commands and executes async I/O
//! 3. Tokio worker pushes completions to lock-free queue
//! 4. SPDK poller checks queue and completes I/O

use bytes::Bytes;
use crossbeam_queue::SegQueue;
use foundry::VolumeBackend;
use std::sync::Arc;
use tokio::sync::mpsc::UnboundedSender;

/// Commands sent from SPDK thread to Tokio runtime.
pub enum NvmeCommand {
    /// Read request from NVMe initiator.
    Read {
        /// Byte offset in volume.
        offset: u64,
        /// Number of bytes to read.
        len: u64,
        /// Opaque pointer to SPDK IO structure (for completion).
        io_ptr: *mut std::ffi::c_void,
    },
    /// Write request from NVMe initiator.
    Write {
        /// Byte offset in volume.
        offset: u64,
        /// Data to write (owned copy from SPDK iovec).
        data: Bytes,
        /// Opaque pointer to SPDK IO structure (for completion).
        io_ptr: *mut std::ffi::c_void,
    },
}

// Safety: NvmeCommand contains a raw pointer, but we control its lifecycle.
// The pointer is only used on the SPDK thread after the async work completes.
unsafe impl Send for NvmeCommand {}

/// Completion results sent from Tokio runtime to SPDK poller.
pub enum NvmeCompletion {
    /// I/O succeeded.
    Success {
        /// Opaque pointer to SPDK IO structure.
        io_ptr: *mut std::ffi::c_void,
        /// Read data (None for writes).
        data: Option<Bytes>,
    },
    /// I/O failed.
    Error {
        /// Opaque pointer to SPDK IO structure.
        io_ptr: *mut std::ffi::c_void,
        /// Error code (negative errno-style).
        status: i32,
    },
}

// Safety: Same reasoning as NvmeCommand.
unsafe impl Send for NvmeCompletion {}

/// The bridge between SPDK and Tokio.
///
/// This structure is created once per volume and manages the async boundary.
pub struct IoBridge {
    /// Sender for submitting commands to the Tokio runtime.
    sender: UnboundedSender<NvmeCommand>,
    /// Lock-free queue for receiving completions from Tokio.
    completion_queue: Arc<SegQueue<NvmeCompletion>>,
}

impl IoBridge {
    /// Create a new I/O bridge for the given volume.
    ///
    /// This spawns a Tokio task that processes commands and pushes completions.
    ///
    /// # Arguments
    ///
    /// * `volume` - The Foundry volume backend to forward I/O to.
    pub fn new(volume: Arc<dyn VolumeBackend>) -> Self {
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        let queue = Arc::new(SegQueue::new());
        let queue_clone = queue.clone();

        // Spawn the Foundry worker on the Tokio runtime.
        tokio::spawn(async move {
            while let Some(cmd) = rx.recv().await {
                // Convert io_ptr to usize to make it Send-safe across await
                // We'll convert it back to a pointer after the async operation
                let (io_ptr_addr, io_result) = match cmd {
                    NvmeCommand::Read {
                        offset,
                        len,
                        io_ptr,
                    } => {
                        let ptr_addr = io_ptr as usize;
                        tracing::trace!(offset, len, "Processing NVMe Read command");
                        let result = volume.read_at(offset, len as usize).await;
                        let io_result = match result {
                            Ok(data) => {
                                tracing::trace!(
                                    offset,
                                    len,
                                    data_len = data.len(),
                                    "Read succeeded"
                                );
                                Ok(Some(data))
                            }
                            Err(e) => {
                                tracing::error!(offset, len, error = %e, "Read failed");
                                Err(-5) // -EIO
                            }
                        };
                        (ptr_addr, io_result)
                    }
                    NvmeCommand::Write {
                        offset,
                        data,
                        io_ptr,
                    } => {
                        let ptr_addr = io_ptr as usize;
                        let len = data.len();
                        tracing::trace!(offset, len, "Processing NVMe Write command");
                        let result = volume.write_at(offset, data).await;
                        let io_result = match result {
                            Ok(_) => {
                                tracing::trace!(offset, len, "Write succeeded");
                                Ok(None)
                            }
                            Err(e) => {
                                tracing::error!(offset, len, error = %e, "Write failed");
                                Err(-5) // -EIO
                            }
                        };
                        (ptr_addr, io_result)
                    }
                };

                // Convert back to pointer after await
                let io_ptr = io_ptr_addr as *mut std::ffi::c_void;

                // Create completion
                let completion = match io_result {
                    Ok(data) => NvmeCompletion::Success { io_ptr, data },
                    Err(status) => NvmeCompletion::Error { io_ptr, status },
                };

                // Push completion to lock-free queue for SPDK poller.
                queue_clone.push(completion);
            }

            tracing::info!("I/O bridge worker terminated");
        });

        Self {
            sender: tx,
            completion_queue: queue,
        }
    }

    /// Submit an I/O command from the SPDK thread.
    ///
    /// This is called from the SPDK bdev callbacks (read/write).
    ///
    /// # Safety
    ///
    /// The caller must ensure that `io_ptr` in the command remains valid until
    /// the corresponding completion is processed.
    pub fn submit(&self, cmd: NvmeCommand) {
        if self.sender.send(cmd).is_err() {
            tracing::error!("Failed to submit command: Tokio worker has terminated");
        }
    }

    /// Poll for completions from the Tokio runtime.
    ///
    /// This is called from the SPDK poller on every tick.
    ///
    /// # Returns
    ///
    /// The next completion if available, or None if the queue is empty.
    pub fn poll_completions(&self) -> Option<NvmeCompletion> {
        self.completion_queue.pop()
    }
}

// Tests require SPDK to be linked, so we only compile them when the actual SPDK
// library is available. In practice, these would run in a Linux environment with
// SPDK installed.
#[cfg(all(test, feature = "spdk-tests"))]
mod tests {
    use super::*;
    use foundry::{BackendType, Foundry, VolumeId};
    use tempfile::TempDir;

    #[tokio::test]
    async fn test_bridge_read_write() {
        // Create a test volume
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());
        let volume_id = VolumeId::new();
        let volume = foundry
            .create_volume(volume_id, 1024 * 1024, Some(BackendType::Legacy))
            .await
            .unwrap();

        // Create bridge
        let bridge = IoBridge::new(volume.clone());

        // Simulate SPDK write (use dummy pointer for test)
        let io_ptr = 0x1234 as *mut std::ffi::c_void;
        let data = Bytes::from(vec![0x42; 4096]);
        bridge.submit(NvmeCommand::Write {
            offset: 0,
            data: data.clone(),
            io_ptr,
        });

        // Poll for completion
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let completion = bridge.poll_completions();
        assert!(matches!(completion, Some(NvmeCompletion::Success { .. })));

        // Simulate SPDK read
        let io_ptr = 0x5678 as *mut std::ffi::c_void;
        bridge.submit(NvmeCommand::Read {
            offset: 0,
            len: 4096,
            io_ptr,
        });

        // Poll for completion
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let completion = bridge.poll_completions().unwrap();
        match completion {
            NvmeCompletion::Success {
                data: Some(read_data),
                ..
            } => {
                assert_eq!(read_data.len(), 4096);
                assert_eq!(read_data[0], 0x42);
            }
            _ => panic!("Expected successful read"),
        }
    }

    #[tokio::test]
    async fn test_bridge_out_of_bounds() {
        let temp_dir = TempDir::new().unwrap();
        let foundry = Foundry::with_data_dir(temp_dir.path());
        let volume_id = VolumeId::new();
        let volume = foundry
            .create_volume(volume_id, 4096, Some(BackendType::Legacy))
            .await
            .unwrap();

        let bridge = IoBridge::new(volume);

        // Try to read beyond volume size
        let io_ptr = 0x9999 as *mut std::ffi::c_void;
        bridge.submit(NvmeCommand::Read {
            offset: 8192,
            len: 4096,
            io_ptr,
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
        let completion = bridge.poll_completions().unwrap();
        assert!(matches!(completion, NvmeCompletion::Error { .. }));
    }
}
