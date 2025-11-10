//! Advanced security primitives (Phase 3.3).
//!
//! Each submodule is feature-gated under `advanced-security` so it can be
//! composed independently by higher-level crates without polluting their core
//! logic.

pub mod audit_log;
pub mod bloom_dedup;
pub mod crypto_profiles;
pub mod ebpf_gateway;

pub use audit_log::{AuditLog, AuditLogBuilder, AuditRecord, AuditTrail, TsaClient, TsaProof};
pub use bloom_dedup::{BloomFilterWrapper, BloomStats, DedupOptimizer};
pub use crypto_profiles::{
    HybridKeyMaterial, MlkemKeyManager, MlkemKeyMaterialState, MlkemNonceExt,
};
pub use ebpf_gateway::{
    EbpfGateway, MtlsLayer, MtlsRejection, SpiffeIdentity, SpiffeWorkloadClient, ZeroTrustConfig,
};
