use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TieringConfig {
    pub scan_interval: Duration,
    pub cold_threshold: Duration,
    pub max_segments_per_scan: usize,
}

impl TieringConfig {
    pub fn default_scan() -> Self {
        Self {
            scan_interval: Duration::from_secs(60),
            cold_threshold: Duration::from_secs(60 * 60 * 24 * 30),
            max_segments_per_scan: 256,
        }
    }
}
