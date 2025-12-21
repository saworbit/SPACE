use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use common::CapsuleId;

#[derive(Debug, Clone)]
pub struct AccessMetrics {
    pub last_access: SystemTime,
    pub access_count: u64,
}

impl AccessMetrics {
    fn new(now: SystemTime) -> Self {
        Self {
            last_access: now,
            access_count: 1,
        }
    }
}

#[derive(Clone, Default)]
pub struct Heatmap {
    inner: Arc<Mutex<HashMap<CapsuleId, AccessMetrics>>>,
}

impl Heatmap {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn touch(&self, id: CapsuleId) {
        let now = SystemTime::now();
        let mut guard = self.inner.lock().expect("heatmap mutex poisoned");
        match guard.get_mut(&id) {
            Some(metrics) => {
                metrics.last_access = now;
                metrics.access_count = metrics.access_count.saturating_add(1);
            }
            None => {
                guard.insert(id, AccessMetrics::new(now));
            }
        }
    }

    pub fn get_metrics(&self, id: CapsuleId) -> Option<AccessMetrics> {
        let guard = self.inner.lock().expect("heatmap mutex poisoned");
        guard.get(&id).cloned()
    }

    pub fn cold_candidates(&self, threshold: Duration, limit: usize) -> Vec<CapsuleId> {
        let now = SystemTime::now();
        let guard = self.inner.lock().expect("heatmap mutex poisoned");
        let mut items: Vec<(CapsuleId, SystemTime)> = guard
            .iter()
            .filter_map(|(id, metrics)| {
                let age = now.duration_since(metrics.last_access).ok()?;
                if age >= threshold {
                    Some((*id, metrics.last_access))
                } else {
                    None
                }
            })
            .collect();

        items.sort_by_key(|(_, last_access)| *last_access);
        items.into_iter().take(limit).map(|(id, _)| id).collect()
    }
}
