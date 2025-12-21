use std::path::PathBuf;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct TieringConfig {
    pub scan_interval: Duration,
    pub cold_threshold: Duration,
    pub max_capsules_per_scan: usize,
    pub hot_root: PathBuf,
    pub cold_root: PathBuf,
    pub reheat_on_read: bool,
}

impl TieringConfig {
    pub fn simulated_s3(hot_root: PathBuf, cold_root: PathBuf) -> Self {
        Self {
            scan_interval: Duration::from_secs(60),
            cold_threshold: Duration::from_secs(60 * 60 * 24 * 30),
            max_capsules_per_scan: 128,
            hot_root,
            cold_root,
            reheat_on_read: false,
        }
    }
}
