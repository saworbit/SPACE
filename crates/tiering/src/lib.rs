mod agent;
mod config;
mod heatmap;
mod mover;
mod stub;

#[cfg(feature = "audit")]
mod audit;

pub use agent::{spawn_tiering_agent, TieringAgentHandle};
pub use config::TieringConfig;
pub use heatmap::{AccessMetrics, Heatmap};
pub use mover::{migrate_segment_to_cold, recall_segment_from_cold, TieringPaths};
pub use stub::{SegmentStub, StubBackend};

#[cfg(feature = "audit")]
pub use audit::{spawn_audit_heatmap_watcher, AuditWatcherHandle};
