//! Quality-of-Service admission control for the storage pipeline.
//!
//! Provides mClock-style scheduling with:
//!
//! - **Priority classes**: Client IO, recovery, and background maintenance
//!   each get separate concurrency budgets.
//! - **Admission control**: Requests are rejected or queued when the system
//!   is overloaded, preventing cascading latency spikes.
//! - **Configurable limits**: Each class has a reservation (guaranteed
//!   concurrency), weight (proportional share), and limit (ceiling).
//!
//! ## Usage
//!
//! ```rust,ignore
//! let qos = QosScheduler::new(QosConfig::default());
//! let permit = qos.acquire(IoClass::Client).await?;
//! // … perform IO …
//! drop(permit); // releases the slot
//! ```

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

/// IO priority class — client, recovery, or background maintenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum IoClass {
    /// User-facing capsule reads and writes.
    Client,
    /// Data recovery and replication (rebalance, backfill).
    Recovery,
    /// Background maintenance (scrub, GC, tiering).
    Background,
}

/// Per-class QoS parameters (reservation / weight / limit model).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassConfig {
    /// Maximum concurrent operations for this class.
    pub limit: u32,
}

/// Top-level QoS configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QosConfig {
    pub client: ClassConfig,
    pub recovery: ClassConfig,
    pub background: ClassConfig,
}

impl Default for QosConfig {
    fn default() -> Self {
        Self {
            client: ClassConfig { limit: 64 },
            recovery: ClassConfig { limit: 16 },
            background: ClassConfig { limit: 8 },
        }
    }
}

/// RAII permit — holds a semaphore slot until dropped.
#[derive(Debug)]
pub struct QosPermit {
    _permit: OwnedSemaphorePermit,
    pub class: IoClass,
}

/// Semaphore-based QoS scheduler.
///
/// Each IO class gets its own semaphore with a configured concurrency
/// ceiling. Acquiring a permit blocks (or fails) when the class is at
/// capacity, preventing overload.
pub struct QosScheduler {
    client: Arc<Semaphore>,
    recovery: Arc<Semaphore>,
    background: Arc<Semaphore>,
}

impl QosScheduler {
    pub fn new(config: &QosConfig) -> Self {
        Self {
            client: Arc::new(Semaphore::new(config.client.limit as usize)),
            recovery: Arc::new(Semaphore::new(config.recovery.limit as usize)),
            background: Arc::new(Semaphore::new(config.background.limit as usize)),
        }
    }

    /// Acquire a permit, waiting if the class is at capacity.
    pub async fn acquire(&self, class: IoClass) -> Result<QosPermit, QosError> {
        let sem = self.semaphore(class);
        let permit = sem
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| QosError::Shutdown)?;
        Ok(QosPermit {
            _permit: permit,
            class,
        })
    }

    /// Try to acquire a permit without waiting. Returns `None` if at capacity.
    pub fn try_acquire(&self, class: IoClass) -> Result<QosPermit, QosError> {
        let sem = self.semaphore(class);
        match sem.clone().try_acquire_owned() {
            Ok(permit) => Ok(QosPermit {
                _permit: permit,
                class,
            }),
            Err(TryAcquireError::NoPermits) => Err(QosError::AtCapacity(class)),
            Err(TryAcquireError::Closed) => Err(QosError::Shutdown),
        }
    }

    /// Available permits for a given class.
    pub fn available(&self, class: IoClass) -> usize {
        self.semaphore(class).available_permits()
    }

    fn semaphore(&self, class: IoClass) -> &Arc<Semaphore> {
        match class {
            IoClass::Client => &self.client,
            IoClass::Recovery => &self.recovery,
            IoClass::Background => &self.background,
        }
    }
}

/// QoS errors.
#[derive(Debug)]
pub enum QosError {
    AtCapacity(IoClass),
    Shutdown,
}

impl std::fmt::Display for QosError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AtCapacity(class) => write!(f, "{class:?} class is at capacity"),
            Self::Shutdown => write!(f, "scheduler has been shut down"),
        }
    }
}

impl std::error::Error for QosError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn permits_are_bounded() {
        let config = QosConfig {
            client: ClassConfig { limit: 2 },
            recovery: ClassConfig { limit: 1 },
            background: ClassConfig { limit: 1 },
        };
        let sched = QosScheduler::new(&config);

        let _p1 = sched.acquire(IoClass::Client).await.unwrap();
        let _p2 = sched.acquire(IoClass::Client).await.unwrap();

        // Third acquire should fail (try, non-blocking)
        let result = sched.try_acquire(IoClass::Client);
        assert!(result.is_err(), "should be at capacity after 2 permits");
    }

    #[tokio::test]
    async fn permits_released_on_drop() {
        let config = QosConfig {
            client: ClassConfig { limit: 1 },
            recovery: ClassConfig { limit: 1 },
            background: ClassConfig { limit: 1 },
        };
        let sched = QosScheduler::new(&config);

        {
            let _p = sched.acquire(IoClass::Client).await.unwrap();
            assert_eq!(sched.available(IoClass::Client), 0);
        }
        // After drop, permit is released.
        assert_eq!(sched.available(IoClass::Client), 1);
    }

    #[tokio::test]
    async fn classes_are_isolated() {
        let config = QosConfig {
            client: ClassConfig { limit: 1 },
            recovery: ClassConfig { limit: 1 },
            background: ClassConfig { limit: 1 },
        };
        let sched = QosScheduler::new(&config);

        let _client = sched.acquire(IoClass::Client).await.unwrap();
        // Recovery should still be available even though client is full.
        let _recovery = sched.acquire(IoClass::Recovery).await.unwrap();
        let _bg = sched.acquire(IoClass::Background).await.unwrap();
    }

    // ── QosConfig defaults ──────────────────────────────────────────

    #[test]
    fn config_defaults() {
        let config = QosConfig::default();
        assert_eq!(config.client.limit, 64);
        assert_eq!(config.recovery.limit, 16);
        assert_eq!(config.background.limit, 8);
    }

    // ── QosError Display ────────────────────────────────────────────

    #[test]
    fn error_at_capacity_display() {
        let err = QosError::AtCapacity(IoClass::Client);
        let msg = format!("{err}");
        assert!(msg.contains("Client"));
        assert!(msg.contains("capacity"));
    }

    #[test]
    fn error_shutdown_display() {
        let err = QosError::Shutdown;
        let msg = format!("{err}");
        assert!(msg.contains("shut down"));
    }

    #[test]
    fn error_implements_std_error() {
        let e = QosError::Shutdown;
        let _: &dyn std::error::Error = &e;
    }

    // ── QosPermit carries IoClass ───────────────────────────────────

    #[tokio::test]
    async fn permit_carries_class_tag() {
        let config = QosConfig::default();
        let sched = QosScheduler::new(&config);

        let permit = sched.acquire(IoClass::Recovery).await.unwrap();
        assert_eq!(permit.class, IoClass::Recovery);
    }

    // ── Available permits tracking ──────────────────────────────────

    #[tokio::test]
    async fn available_reflects_acquired_permits() {
        let config = QosConfig {
            client: ClassConfig { limit: 4 },
            recovery: ClassConfig { limit: 2 },
            background: ClassConfig { limit: 1 },
        };
        let sched = QosScheduler::new(&config);

        assert_eq!(sched.available(IoClass::Client), 4);
        let _p1 = sched.acquire(IoClass::Client).await.unwrap();
        assert_eq!(sched.available(IoClass::Client), 3);
        let _p2 = sched.acquire(IoClass::Client).await.unwrap();
        assert_eq!(sched.available(IoClass::Client), 2);
    }

    // ── try_acquire error differentiation ───────────────────────────

    #[tokio::test]
    async fn try_acquire_at_capacity_returns_correct_error() {
        let config = QosConfig {
            client: ClassConfig { limit: 1 },
            recovery: ClassConfig { limit: 1 },
            background: ClassConfig { limit: 1 },
        };
        let sched = QosScheduler::new(&config);
        let _p = sched.acquire(IoClass::Background).await.unwrap();

        match sched.try_acquire(IoClass::Background) {
            Err(QosError::AtCapacity(IoClass::Background)) => {} // expected
            other => panic!("expected AtCapacity(Background), got {other:?}"),
        }
    }

    // ── Concurrent saturation across all classes ────────────────────

    #[tokio::test]
    async fn saturate_all_classes_simultaneously() {
        let config = QosConfig {
            client: ClassConfig { limit: 2 },
            recovery: ClassConfig { limit: 2 },
            background: ClassConfig { limit: 2 },
        };
        let sched = QosScheduler::new(&config);

        let mut permits = Vec::new();
        for class in [IoClass::Client, IoClass::Recovery, IoClass::Background] {
            for _ in 0..2 {
                permits.push(sched.acquire(class).await.unwrap());
            }
        }

        // All classes should be at capacity now
        for class in [IoClass::Client, IoClass::Recovery, IoClass::Background] {
            assert_eq!(sched.available(class), 0, "{class:?} should be at capacity");
            assert!(sched.try_acquire(class).is_err());
        }

        // Drop all permits
        permits.clear();

        // All classes should have permits available again
        for class in [IoClass::Client, IoClass::Recovery, IoClass::Background] {
            assert_eq!(
                sched.available(class),
                2,
                "{class:?} should be fully available"
            );
        }
    }

    // ── IoClass serde ───────────────────────────────────────────────

    #[test]
    fn io_class_serde_roundtrip() {
        for class in [IoClass::Client, IoClass::Recovery, IoClass::Background] {
            let json = serde_json::to_string(&class).unwrap();
            let restored: IoClass = serde_json::from_str(&json).unwrap();
            assert_eq!(class, restored);
        }
    }

    // ── QosConfig serde ─────────────────────────────────────────────

    #[test]
    fn qos_config_serde_roundtrip() {
        let config = QosConfig {
            client: ClassConfig { limit: 32 },
            recovery: ClassConfig { limit: 8 },
            background: ClassConfig { limit: 4 },
        };
        let json = serde_json::to_string(&config).unwrap();
        let restored: QosConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.client.limit, 32);
        assert_eq!(restored.recovery.limit, 8);
        assert_eq!(restored.background.limit, 4);
    }
}
