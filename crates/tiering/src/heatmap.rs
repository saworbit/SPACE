use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dashmap::DashMap;
use serde::{Deserialize, Serialize};

use common::SegmentId;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessStats {
    pub last_accessed: u64, // Unix timestamp
    pub access_count: u64,
}

#[derive(Clone, Default)]
pub struct Heatmap {
    // In production, back this with Sled. For now, memory is fine.
    stats: Arc<DashMap<SegmentId, AccessStats>>,
}

impl Heatmap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_access(&self, segment_id: SegmentId) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        self.stats
            .entry(segment_id)
            .and_modify(|s| {
                s.last_accessed = now;
                s.access_count = s.access_count.saturating_add(1);
            })
            .or_insert(AccessStats {
                last_accessed: now,
                access_count: 1,
            });
    }

    pub fn get_stats(&self, segment_id: SegmentId) -> Option<AccessStats> {
        self.stats.get(&segment_id).map(|r| r.value().clone())
    }

    pub fn get_cold_candidates(&self, age_seconds: u64, limit: usize) -> Vec<SegmentId> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let mut items: Vec<(SegmentId, u64)> = self
            .stats
            .iter()
            .filter(|r| now.saturating_sub(r.value().last_accessed) > age_seconds)
            .map(|r| (*r.key(), r.value().last_accessed))
            .collect();

        items.sort_by_key(|(_, last_accessed)| *last_accessed);
        items.into_iter().take(limit).map(|(id, _)| id).collect()
    }

    pub fn cold_candidates(&self, threshold: Duration, limit: usize) -> Vec<SegmentId> {
        self.get_cold_candidates(threshold.as_secs(), limit)
    }
}
