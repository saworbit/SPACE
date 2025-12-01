//! Async Batching Queue for Geo-Replication
//!
//! Implements batched replication with configurable RPO intervals.
//! Segments are queued and flushed at RPO-based intervals to minimize
//! network overhead while meeting durability guarantees.

use anyhow::Result;
use common::{CapsuleId, SegmentId};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::{mpsc, RwLock};
use tokio::time::interval;
use tracing::{info, warn};

/// Default byte cap for a batch when callers do not specify a limit (4 MiB)
pub const DEFAULT_MAX_BATCH_BYTES: usize = 4 * 1024 * 1024;

/// Batch queue item representing a segment to be replicated
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub capsule_id: CapsuleId,
    pub segment_id: SegmentId,
    pub segment_data: Vec<u8>,
}

impl BatchItem {
    /// Returns approximate memory usage for triggering byte-based flushes
    pub fn size_bytes(&self) -> usize {
        self.segment_data.len()
            + std::mem::size_of::<CapsuleId>()
            + std::mem::size_of::<SegmentId>()
    }
}

/// Async batching queue for geo-replication
///
/// Collects segments and flushes them at configured intervals
/// to balance network efficiency with RPO guarantees.
pub struct BatchQueue {
    /// Pending items to be replicated
    pending: Arc<RwLock<Vec<BatchItem>>>,
    /// Receiver for incoming batch items
    rx: mpsc::UnboundedReceiver<BatchItem>,
    /// Interval for batch flushes
    flush_interval: Duration,
    /// Maximum batch size before forced flush
    max_batch_size: usize,
    /// Maximum bytes before forced flush
    max_batch_bytes: usize,
}

impl BatchQueue {
    /// Create a new batch queue with specified flush interval
    pub fn new(
        flush_interval: Duration,
        max_batch_size: usize,
        max_batch_bytes: usize,
    ) -> (Self, BatchQueueSender) {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending = Arc::new(RwLock::new(Vec::new()));

        let queue = Self {
            pending: pending.clone(),
            rx,
            flush_interval,
            max_batch_size,
            max_batch_bytes,
        };

        let sender = BatchQueueSender { tx, pending };

        (queue, sender)
    }

    /// Convenience constructor using a sane default byte ceiling (4 MiB)
    pub fn new_with_defaults(
        flush_interval: Duration,
        max_batch_size: usize,
    ) -> (Self, BatchQueueSender) {
        Self::new(flush_interval, max_batch_size, DEFAULT_MAX_BATCH_BYTES)
    }

    /// Run the batch queue, flushing at configured intervals
    ///
    /// # Arguments
    /// * `flush_fn` - Async function to execute when flushing batches
    pub async fn run<F, Fut>(mut self, flush_fn: F) -> Result<()>
    where
        F: Fn(Vec<BatchItem>) -> Fut,
        Fut: std::future::Future<Output = Result<()>>,
    {
        let mut ticker = interval(self.flush_interval);
        ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // Consume the immediate first tick so the next tick respects flush_interval
        ticker.tick().await;

        // Tracks byte footprint of the pending batch without repeatedly walking the vector
        let mut current_batch_bytes = 0usize;

        info!(
            interval_secs = self.flush_interval.as_secs(),
            max_batch_size = self.max_batch_size,
            max_batch_bytes = self.max_batch_bytes,
            "batch queue started"
        );

        loop {
            tokio::select! {
                // Incoming batch item
                item = self.rx.recv() => {
                    match item {
                        Some(item) => {
                            let item_size = item.size_bytes();
                            let mut pending = self.pending.write().await;
                            pending.push(item);

                            current_batch_bytes += item_size;

                            // Hybrid triggers: count OR byte ceiling
                            let count_limit_hit = pending.len() >= self.max_batch_size;
                            let byte_limit_hit = current_batch_bytes >= self.max_batch_bytes;

                            if count_limit_hit || byte_limit_hit {
                                let batch = std::mem::take(&mut *pending);
                                let flushed_len = batch.len();
                                let flushed_bytes = current_batch_bytes;
                                drop(pending);

                                // Reset tracker post-flush
                                current_batch_bytes = 0;

                                info!(
                                    batch_size = flushed_len,
                                    batch_bytes = flushed_bytes,
                                    reason = if count_limit_hit { "count_limit" } else { "byte_limit" },
                                    "flushing batch"
                                );

                                if let Err(e) = flush_fn(batch).await {
                                    warn!(error = %e, "failed to flush batch");
                                }
                            }
                        }
                        None => {
                            info!("batch queue channel closed, shutting down");
                            break;
                        }
                    }
                }

                // Periodic flush
                _ = ticker.tick() => {
                    let mut pending = self.pending.write().await;
                    if !pending.is_empty() {
                        let batch = std::mem::take(&mut *pending);
                        let flushed_len = batch.len();
                        let flushed_bytes = current_batch_bytes;
                        drop(pending);

                        // Reset tracker for next interval window
                        current_batch_bytes = 0;

                        info!(
                            batch_size = flushed_len,
                            batch_bytes = flushed_bytes,
                            "flushing batch (interval tick)"
                        );

                        if let Err(e) = flush_fn(batch).await {
                            warn!(error = %e, "failed to flush batch");
                        }
                    }
                }

                // Channel closed
                else => {
                    info!("batch queue channel closed, shutting down");
                    break;
                }
            }
        }

        Ok(())
    }
}

/// Sender handle for the batch queue
///
/// Allows enqueueing items without blocking
#[derive(Clone)]
pub struct BatchQueueSender {
    tx: mpsc::UnboundedSender<BatchItem>,
    pending: Arc<RwLock<Vec<BatchItem>>>,
}

impl BatchQueueSender {
    /// Enqueue an item for batched replication
    pub fn enqueue(&self, item: BatchItem) -> Result<()> {
        self.tx
            .send(item)
            .map_err(|e| anyhow::anyhow!("failed to enqueue item: {}", e))?;
        Ok(())
    }

    /// Get current queue depth
    pub async fn queue_depth(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Get statistics about queued capsules
    pub async fn stats(&self) -> QueueStats {
        let pending = self.pending.read().await;
        let total_items = pending.len();

        let mut capsule_counts: HashMap<CapsuleId, usize> = HashMap::new();
        let mut total_bytes = 0;

        for item in pending.iter() {
            *capsule_counts.entry(item.capsule_id).or_insert(0) += 1;
            total_bytes += item.size_bytes();
        }

        QueueStats {
            total_items,
            unique_capsules: capsule_counts.len(),
            total_bytes,
        }
    }
}

/// Statistics about the batch queue
#[derive(Debug, Clone)]
pub struct QueueStats {
    pub total_items: usize,
    pub unique_capsules: usize,
    pub total_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_batch_queue_interval_flush() {
        let (queue, sender) = BatchQueue::new_with_defaults(Duration::from_millis(100), 1000);

        // Track flushed batches
        let flushed = Arc::new(RwLock::new(Vec::new()));
        let flushed_clone = flushed.clone();

        // Start queue in background
        let queue_handle = tokio::spawn(async move {
            queue
                .run(|batch| {
                    let flushed = flushed_clone.clone();
                    async move {
                        let mut f = flushed.write().await;
                        f.push(batch.len());
                        Ok(())
                    }
                })
                .await
        });

        // Enqueue items
        for i in 0..5 {
            sender
                .enqueue(BatchItem {
                    capsule_id: CapsuleId::new(),
                    segment_id: SegmentId(i),
                    segment_data: vec![0u8; 100],
                })
                .unwrap();
        }

        // Wait for interval flush
        tokio::time::sleep(Duration::from_millis(150)).await;

        // Check that batch was flushed
        let flushed_counts = flushed.read().await;
        assert_eq!(flushed_counts.len(), 1);
        assert_eq!(flushed_counts[0], 5);

        // Cleanup
        drop(sender);
        let _ = queue_handle.await;
    }

    #[tokio::test]
    async fn test_batch_queue_size_limit() {
        let (queue, sender) = BatchQueue::new_with_defaults(Duration::from_secs(60), 3);

        let flushed = Arc::new(RwLock::new(Vec::new()));
        let flushed_clone = flushed.clone();

        let queue_handle = tokio::spawn(async move {
            queue
                .run(|batch| {
                    let flushed = flushed_clone.clone();
                    async move {
                        let mut f = flushed.write().await;
                        f.push(batch.len());
                        Ok(())
                    }
                })
                .await
        });

        // Enqueue 3 items - should trigger immediate flush
        for i in 0..3 {
            sender
                .enqueue(BatchItem {
                    capsule_id: CapsuleId::new(),
                    segment_id: SegmentId(i),
                    segment_data: vec![0u8; 100],
                })
                .unwrap();
        }

        // Give it time to flush
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Check that batch was flushed
        let flushed_counts = flushed.read().await;
        assert_eq!(flushed_counts.len(), 1);
        assert_eq!(flushed_counts[0], 3);

        drop(sender);
        let _ = queue_handle.await;
    }

    #[tokio::test]
    async fn test_batch_queue_byte_limit() {
        // Limit: 1000 items, but only 200 bytes allowed
        let (queue, sender) = BatchQueue::new(Duration::from_secs(60), 1000, 200);

        let flushed = Arc::new(RwLock::new(Vec::new()));
        let flushed_clone = flushed.clone();

        let queue_handle = tokio::spawn(async move {
            queue
                .run(|batch| {
                    let flushed = flushed_clone.clone();
                    async move {
                        let mut f = flushed.write().await;
                        f.push(batch.len());
                        Ok(())
                    }
                })
                .await
        });

        // 1. First item (~74 bytes with IDs)
        sender
            .enqueue(BatchItem {
                capsule_id: CapsuleId::new(),
                segment_id: SegmentId(0),
                segment_data: vec![0u8; 50],
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        {
            let f = flushed.read().await;
            assert_eq!(f.len(), 0, "Should not flush yet (bytes < 200)");
        }

        // 2. Second item brings total to ~148 bytes
        sender
            .enqueue(BatchItem {
                capsule_id: CapsuleId::new(),
                segment_id: SegmentId(1),
                segment_data: vec![0u8; 50],
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(10)).await;
        {
            let f = flushed.read().await;
            assert_eq!(f.len(), 0, "Should not flush yet (bytes < 200)");
        }

        // 3. Third large item should cross the byte ceiling
        sender
            .enqueue(BatchItem {
                capsule_id: CapsuleId::new(),
                segment_id: SegmentId(2),
                segment_data: vec![0u8; 100],
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        let flushed_counts = flushed.read().await;
        assert_eq!(
            flushed_counts.len(),
            1,
            "Should have flushed due to byte limit"
        );
        assert_eq!(
            flushed_counts[0], 3,
            "All items should be included in the flush"
        );

        drop(sender);
        let _ = queue_handle.await;
    }

    #[tokio::test]
    async fn test_queue_stats() {
        let (queue, sender) = BatchQueue::new_with_defaults(Duration::from_secs(60), 1000);

        let queue_handle = tokio::spawn(async move { queue.run(|_| async { Ok(()) }).await });

        let capsule_id = CapsuleId::new();

        tokio::time::sleep(Duration::from_millis(10)).await;

        // Enqueue items
        for i in 0..3 {
            sender
                .enqueue(BatchItem {
                    capsule_id,
                    segment_id: SegmentId(i),
                    segment_data: vec![0u8; 100],
                })
                .unwrap();
        }

        // Wait for queue to observe items
        for _ in 0..10 {
            if sender.stats().await.total_items == 3 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        // Check stats
        let stats = sender.stats().await;
        assert_eq!(stats.total_items, 3);
        assert_eq!(stats.unique_capsules, 1);
        // Expect at least the payload size; size_of::<CapsuleId> + size_of::<SegmentId> adds overhead.
        assert!(stats.total_bytes >= 300);

        drop(sender);
        let _ = queue_handle.await;
    }
}
