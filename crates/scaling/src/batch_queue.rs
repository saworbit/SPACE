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

/// Batch queue item representing a segment to be replicated
#[derive(Debug, Clone)]
pub struct BatchItem {
    pub capsule_id: CapsuleId,
    pub segment_id: SegmentId,
    pub segment_data: Vec<u8>,
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
}

impl BatchQueue {
    /// Create a new batch queue with specified flush interval
    pub fn new(flush_interval: Duration, max_batch_size: usize) -> (Self, BatchQueueSender) {
        let (tx, rx) = mpsc::unbounded_channel();
        let pending = Arc::new(RwLock::new(Vec::new()));

        let queue = Self {
            pending: pending.clone(),
            rx,
            flush_interval,
            max_batch_size,
        };

        let sender = BatchQueueSender { tx, pending };

        (queue, sender)
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

        info!(
            interval_secs = self.flush_interval.as_secs(),
            max_batch_size = self.max_batch_size,
            "batch queue started"
        );

        loop {
            tokio::select! {
                // Incoming batch item
                item = self.rx.recv() => {
                    match item {
                        Some(item) => {
                            let mut pending = self.pending.write().await;
                            pending.push(item);

                            // Check if we need to flush due to size limit
                            if pending.len() >= self.max_batch_size {
                                let batch = std::mem::take(&mut *pending);
                                drop(pending);

                                info!(
                                    batch_size = batch.len(),
                                    "flushing batch (size limit reached)"
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
                        drop(pending);

                        info!(
                            batch_size = batch.len(),
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
            total_bytes += item.segment_data.len();
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
        let (queue, sender) = BatchQueue::new(Duration::from_millis(100), 1000);

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
        let (queue, sender) = BatchQueue::new(Duration::from_secs(60), 3);

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
    async fn test_queue_stats() {
        let (queue, sender) = BatchQueue::new(Duration::from_secs(60), 1000);

        let queue_handle = tokio::spawn(async move {
            queue
                .run(|_| async { Ok(()) })
                .await
        });

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
        assert_eq!(stats.total_bytes, 300);

        drop(sender);
        let _ = queue_handle.await;
    }
}
