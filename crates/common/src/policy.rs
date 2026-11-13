use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Cryptography profile for the write pipeline.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
pub enum CryptoProfile {
    /// Classical AES + BLAKE3 path.
    #[default]
    Classical,
    /// Hybrid Kyber (ML-KEM) + AES for post-quantum readiness.
    HybridKyber,
}

/// Compression algorithm selection
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum CompressionPolicy {
    /// No compression
    None,
    /// LZ4 fast compression (level 1-16)
    LZ4 { level: i32 },
    /// Zstd balanced compression (level 1-22)
    Zstd { level: i32 },
}

impl Default for CompressionPolicy {
    fn default() -> Self {
        // Default to LZ4 level 1 for speed
        CompressionPolicy::LZ4 { level: 1 }
    }
}

/// Encryption policy
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub enum EncryptionPolicy {
    /// No encryption
    #[default]
    Disabled,
    /// XTS-AES-256 with specified key version
    XtsAes256 { key_version: Option<u32> },
}

impl EncryptionPolicy {
    /// Check if encryption is enabled
    pub fn is_enabled(&self) -> bool {
        !matches!(self, EncryptionPolicy::Disabled)
    }

    /// Get the key version to use (None = use current/latest)
    pub fn key_version(&self) -> Option<u32> {
        match self {
            EncryptionPolicy::Disabled => None,
            EncryptionPolicy::XtsAes256 { key_version } => *key_version,
        }
    }
}

/// Layout strategy choice for CapsuleFlow.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum LayoutStrategy {
    Fixed {
        segment_size: usize,
    },
    AdaptiveEntropy,
    ZnsGraph {
        zone_size_mib: u32,
        graph_radius: u32,
    },
    Learned {
        model_path: String,
    },
    QuantumReady {
        merkle_algo: MerkleAlgo,
    },
}

/// Merkle variant for QuantumReady mode.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum MerkleAlgo {
    Blake3,
    SphincsPlus,
}

/// Federation strategy for distributing capsules across zones.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum FederationStrategy {
    /// Replicate capsule metadata + data into the target zones.
    ReplicateTo,
    /// Move capsule ownership into the target zones (replicate then remove locally).
    MoveTo,
    /// Cache capsule data at the target zones (best-effort).
    CacheAt,
}

/// Priority class for inter-zone transfers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TransferPriority {
    /// Best-effort replication via background queue.
    #[default]
    Background,
    /// Urgent replication that should bypass normal scheduling.
    Critical,
}

/// Federation directives attached to a capsule policy.
///
/// Accepts multiple wire formats for backwards-compatibility:
/// - `federation: ["us-west"]`
/// - `federation: { targets: ["us-west"], priority: critical }`
/// - legacy `federation: { target_zones: ["us-west"], strategy: replicate-to }`
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct FederationPolicy {
    /// List of target Zone IDs or aliases (e.g., "eu-central-1").
    #[serde(default)]
    pub targets: Vec<String>,
    /// Priority of transfer (Background vs Critical).
    #[serde(default)]
    pub priority: TransferPriority,
    /// Legacy strategy knob retained for compatibility (ignored by Phase 4b replication).
    #[serde(default)]
    pub strategy: Option<FederationStrategy>,
}

impl Default for FederationPolicy {
    fn default() -> Self {
        Self {
            targets: Vec::new(),
            priority: TransferPriority::Background,
            strategy: None,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum FederationPolicyRepr {
    Targets(Vec<String>),
    Fields(FederationPolicyFields),
}

#[derive(Debug, Clone, Deserialize)]
struct FederationPolicyFields {
    #[serde(default, alias = "target_zones")]
    targets: Vec<String>,
    #[serde(default)]
    priority: TransferPriority,
    #[serde(default)]
    strategy: Option<FederationStrategy>,
}

impl<'de> Deserialize<'de> for FederationPolicy {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        if deserializer.is_human_readable() {
            match FederationPolicyRepr::deserialize(deserializer)? {
                FederationPolicyRepr::Targets(targets) => Ok(Self {
                    targets,
                    ..Self::default()
                }),
                FederationPolicyRepr::Fields(fields) => Ok(Self {
                    targets: fields.targets,
                    priority: fields.priority,
                    strategy: fields.strategy,
                }),
            }
        } else {
            let fields = FederationPolicyFields::deserialize(deserializer)?;
            Ok(Self {
                targets: fields.targets,
                priority: fields.priority,
                strategy: fields.strategy,
            })
        }
    }
}

/// Policy knobs for layout planning. Only `strategy` changes anything, so
/// the others needn't be given (#162).
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct LayoutPolicy {
    pub strategy: LayoutStrategy,
    pub ec_profile: (usize, usize),
    pub heat_threshold: f32,
}

impl Default for LayoutPolicy {
    fn default() -> Self {
        LayoutPolicy {
            strategy: LayoutStrategy::Fixed {
                segment_size: 4 * 1024 * 1024,
            },
            ec_profile: (6, 2),
            heat_threshold: 0.7,
        }
    }
}

impl LayoutStrategy {
    pub fn default_segment_size(&self) -> usize {
        match self {
            LayoutStrategy::Fixed { segment_size } => *segment_size,
            LayoutStrategy::AdaptiveEntropy => 4 * 1024 * 1024,
            LayoutStrategy::ZnsGraph { zone_size_mib, .. } => {
                (*zone_size_mib as usize).saturating_mul(1024 * 1024)
            }
            LayoutStrategy::Learned { .. } => 4 * 1024 * 1024,
            LayoutStrategy::QuantumReady { .. } => 4 * 1024 * 1024,
        }
    }
}

/// Storage efficiency policy
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Compression algorithm and level
    pub compression: CompressionPolicy,

    /// Enable inline deduplication
    pub dedupe: bool,

    /// Background compaction interval in seconds (None = disabled)
    pub compact_interval_secs: Option<u64>,

    /// Erasure coding profile (future use)
    pub erasure_profile: Option<String>,

    /// Encryption policy (Phase 3)
    #[serde(default)]
    pub encryption: EncryptionPolicy,

    /// Cryptography profile (Phase 3.3)
    #[serde(default)]
    pub crypto_profile: CryptoProfile,

    /// Layout policy driving CapsuleFlow.
    #[serde(default)]
    pub layout: LayoutPolicy,

    /// Phase 4: optional multi-zone federation rules.
    #[serde(default)]
    pub federation: FederationPolicy,

    /// Phase 5: ordered list of WASM transformations to apply.
    #[serde(default)]
    pub transform: Vec<TransformDef>,
}

/// Ordered transformation definition applied during read/write streaming.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TransformDef {
    /// Friendly name for logging/debugging (e.g., "thumbnail-gen").
    pub name: String,

    /// URI of the WASM binary (e.g., `capsule://{id}`, `file:///opt/space/wasm/...`).
    pub image: String,

    /// Execution trigger. Default: `OnRead`.
    #[serde(default)]
    pub trigger: TransformTrigger,

    /// Static arguments passed into the WASM environment.
    #[serde(default)]
    pub args: HashMap<String, String>,

    /// Resource bounds to prevent denial-of-service.
    #[serde(default)]
    pub resources: ResourceLimits,

    /// Optional artifact verification (content-addressable and/or signature).
    pub verification: Option<ArtifactVerification>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum TransformTrigger {
    /// Lazy execution. Computed when the client pulls data.
    #[default]
    #[serde(
        rename = "on-read",
        alias = "OnRead",
        alias = "on_read",
        alias = "onread"
    )]
    OnRead,
    /// Eager execution. Computed during ingest (before persistence).
    #[serde(
        rename = "on-write",
        alias = "OnWrite",
        alias = "on_write",
        alias = "onwrite"
    )]
    OnWrite,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_memory_pages: u32,
    pub fuel_limit: u64,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_pages: 16,
            fuel_limit: 10_000_000,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtifactVerification {
    pub sha256: String,
    #[serde(default)]
    pub signature: Option<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            compression: CompressionPolicy::default(),
            dedupe: true,
            compact_interval_secs: Some(3600), // 1 hour
            erasure_profile: None,
            encryption: EncryptionPolicy::default(),
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }
}

impl Policy {
    /// Create a policy optimized for text/logs (high compression)
    pub fn text_optimized() -> Self {
        Self {
            compression: CompressionPolicy::Zstd { level: 3 },
            dedupe: true,
            compact_interval_secs: Some(1800),
            erasure_profile: None,
            encryption: EncryptionPolicy::default(),
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }

    /// Create a policy for pre-compressed data (skip compression)
    pub fn precompressed() -> Self {
        Self {
            compression: CompressionPolicy::None,
            dedupe: false,
            compact_interval_secs: Some(7200),
            erasure_profile: None,
            encryption: EncryptionPolicy::default(),
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }

    /// Create a policy for edge nodes (minimal overhead)
    pub fn edge_optimized() -> Self {
        Self {
            compression: CompressionPolicy::LZ4 { level: 1 },
            dedupe: false,
            compact_interval_secs: None, // Manual compaction
            erasure_profile: None,
            encryption: EncryptionPolicy::default(),
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }

    /// Create a policy with encryption enabled
    pub fn encrypted() -> Self {
        Self {
            compression: CompressionPolicy::default(),
            dedupe: true,
            compact_interval_secs: Some(3600),
            erasure_profile: None,
            encryption: EncryptionPolicy::XtsAes256 { key_version: None },
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }

    /// Create a policy with encryption and high compression
    pub fn encrypted_compressed() -> Self {
        Self {
            compression: CompressionPolicy::Zstd { level: 3 },
            dedupe: true,
            compact_interval_secs: Some(3600),
            erasure_profile: None,
            encryption: EncryptionPolicy::XtsAes256 { key_version: None },
            crypto_profile: CryptoProfile::default(),
            layout: LayoutPolicy::default(),
            federation: FederationPolicy::default(),
            transform: Vec::new(),
        }
    }

    /// Reject a policy the write path can't honour. Settings from before
    /// the reset that nothing implements are refused rather than accepted
    /// and ignored, so a capsule never claims a protection it doesn't have
    /// (#162).
    pub fn validate(&self) -> Result<(), PolicyError> {
        if self.crypto_profile != CryptoProfile::Classical {
            return Err(PolicyError::Unsupported(
                "crypto_profile HybridKyber (there is no ML-KEM code)",
            ));
        }
        if !self.transform.is_empty() {
            return Err(PolicyError::Unsupported(
                "transform (there is no WASM runtime)",
            ));
        }
        if self.federation != FederationPolicy::default() {
            return Err(PolicyError::Unsupported(
                "federation (SPACE runs on one node)",
            ));
        }
        if !matches!(self.layout.strategy, LayoutStrategy::Fixed { .. }) {
            return Err(PolicyError::Unsupported(
                "a layout strategy other than Fixed",
            ));
        }
        let size = self.layout.strategy.default_segment_size();
        if size == 0 || size > MAX_SEGMENT_SIZE {
            return Err(PolicyError::SegmentSizeOutOfRange { size });
        }
        Ok(())
    }
}

/// Largest segment a layout may ask for. XTS encrypts each segment as one
/// data unit, and IEEE 1619 caps a data unit at 2^20 blocks (16 MiB).
pub const MAX_SEGMENT_SIZE: usize = 16 * 1024 * 1024;

/// Errors produced by [`Policy::validate`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PolicyError {
    /// A setting nothing in this build implements.
    Unsupported(&'static str),
    SegmentSizeOutOfRange {
        size: usize,
    },
}

impl std::fmt::Display for PolicyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PolicyError::Unsupported(setting) => {
                write!(f, "{setting} is not supported in this build")
            }
            PolicyError::SegmentSizeOutOfRange { size } => write!(
                f,
                "segment size {size} is out of range: it must be between 1 byte and \
                 {MAX_SEGMENT_SIZE} bytes (16 MiB)"
            ),
        }
    }
}

impl std::error::Error for PolicyError {}

/// A policy file, as `spacectl create --policy-file` reads it: the settings
/// the write path honours and nothing else. A misspelled or unknown key is
/// an error instead of being dropped, leaving that setting at its default
/// (#161), and so are the pre-reset settings nothing implements (#162).
/// Settings left out keep the base policy's.
///
/// ```yaml
/// compression: !Zstd { level: 3 }
/// dedupe: false
/// encryption: !XtsAes256 { key_version: null }
/// layout:
///   strategy: !Fixed { segment_size: 1048576 }
/// ```
///
/// [`Policy`] itself stays lenient, since capsule records store it and an
/// older record may hold fields a later build drops.
#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyFile {
    pub compression: Option<CompressionPolicy>,
    pub dedupe: Option<bool>,
    pub encryption: Option<EncryptionPolicy>,
    pub layout: Option<LayoutFile>,
}

/// The `layout` block of a [`PolicyFile`].
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LayoutFile {
    pub strategy: LayoutStrategy,
}

impl PolicyFile {
    /// `base` with the settings this file gives, checked with
    /// [`Policy::validate`].
    pub fn apply(self, base: Policy) -> Result<Policy, PolicyError> {
        let mut policy = base;
        if let Some(compression) = self.compression {
            policy.compression = compression;
        }
        if let Some(dedupe) = self.dedupe {
            policy.dedupe = dedupe;
        }
        if let Some(encryption) = self.encryption {
            policy.encryption = encryption;
        }
        if let Some(layout) = self.layout {
            policy.layout.strategy = layout.strategy;
        }
        policy.validate()?;
        Ok(policy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_policy() {
        let policy = Policy::default();
        assert!(policy.dedupe);
        assert!(matches!(
            policy.compression,
            CompressionPolicy::LZ4 { level: 1 }
        ));
        assert!(matches!(policy.encryption, EncryptionPolicy::Disabled));
    }

    #[test]
    fn test_policy_presets() {
        let text = Policy::text_optimized();
        assert!(matches!(text.compression, CompressionPolicy::Zstd { .. }));

        let precomp = Policy::precompressed();
        assert!(matches!(precomp.compression, CompressionPolicy::None));
        assert!(!precomp.dedupe);

        let edge = Policy::edge_optimized();
        assert!(edge.compact_interval_secs.is_none());
    }

    #[test]
    fn default_policy_validates() {
        Policy::default()
            .validate()
            .expect("default policy must validate");
    }

    #[test]
    fn settings_nothing_implements_are_rejected() {
        let unsupported = |policy: Policy| match policy.validate() {
            Err(PolicyError::Unsupported(setting)) => setting,
            other => panic!("expected Unsupported, got {other:?}"),
        };
        for dedupe in [true, false] {
            let kyber = Policy {
                crypto_profile: CryptoProfile::HybridKyber,
                dedupe,
                ..Policy::default()
            };
            assert!(unsupported(kyber).contains("HybridKyber"));
        }
        let transform: Policy = serde_norway::from_str(
            "transform: [{name: t, image: 'file:///t.wasm', verification: null}]",
        )
        .unwrap();
        assert!(unsupported(transform).contains("transform"));
        let federation: Policy = serde_norway::from_str("federation: [us-west]").unwrap();
        assert!(unsupported(federation).contains("federation"));
        for strategy in [
            LayoutStrategy::AdaptiveEntropy,
            LayoutStrategy::Learned {
                model_path: "m".into(),
            },
            LayoutStrategy::QuantumReady {
                merkle_algo: MerkleAlgo::Blake3,
            },
            LayoutStrategy::ZnsGraph {
                zone_size_mib: 1,
                graph_radius: 1,
            },
        ] {
            let policy = Policy {
                layout: LayoutPolicy {
                    strategy,
                    ..LayoutPolicy::default()
                },
                ..Policy::default()
            };
            assert!(unsupported(policy).contains("layout"));
        }
        assert_eq!(
            PolicyError::Unsupported("x").to_string(),
            "x is not supported in this build"
        );
    }

    #[test]
    fn policy_files_reject_unknown_keys() {
        let parse = |yaml: &str| serde_norway::from_str::<PolicyFile>(yaml);
        for typo in [
            "dedup: false",
            "compresion: None",
            "encrypton: Disabled",
            "crypto_profile: HybridKyber",
            "federation: [us-west]",
            "compact_interval_secs: 60",
            "layout: {strategy: !Fixed {segment_size: 1024}, ec_profile: [6, 2]}",
        ] {
            let err = parse(typo).expect_err(typo).to_string();
            assert!(err.contains("unknown field"), "{typo}: {err}");
        }
    }

    #[test]
    fn policy_files_override_only_what_they_give() {
        let file: PolicyFile = serde_norway::from_str(
            "compression: !Zstd { level: 3 }\n\
             dedupe: false\n\
             layout:\n  strategy: !Fixed { segment_size: 1048576 }\n",
        )
        .unwrap();
        let policy = file.apply(Policy::encrypted()).unwrap();
        assert!(matches!(
            policy.compression,
            CompressionPolicy::Zstd { level: 3 }
        ));
        assert!(!policy.dedupe);
        assert!(policy.encryption.is_enabled(), "kept from the base policy");
        assert_eq!(policy.layout.strategy.default_segment_size(), 1_048_576);

        let file: PolicyFile = serde_norway::from_str("encryption: Disabled").unwrap();
        assert!(!file
            .apply(Policy::encrypted())
            .unwrap()
            .encryption
            .is_enabled());

        let file: PolicyFile =
            serde_norway::from_str("layout: {strategy: !Fixed {segment_size: 0}}").unwrap();
        assert_eq!(
            file.apply(Policy::default()).unwrap_err(),
            PolicyError::SegmentSizeOutOfRange { size: 0 }
        );
        let file: PolicyFile =
            serde_norway::from_str("layout: {strategy: AdaptiveEntropy}").unwrap();
        assert!(matches!(
            file.apply(Policy::default()),
            Err(PolicyError::Unsupported(_))
        ));
    }

    #[test]
    fn a_layout_block_needs_only_its_strategy() {
        let policy: Policy =
            serde_norway::from_str("layout: {strategy: !Fixed {segment_size: 65536}}").unwrap();
        assert_eq!(policy.layout.strategy.default_segment_size(), 65_536);
        assert_eq!(policy.layout.ec_profile, LayoutPolicy::default().ec_profile);
    }

    #[test]
    fn segment_size_must_be_between_one_byte_and_16_mib() {
        let with_layout = |strategy| Policy {
            layout: LayoutPolicy {
                strategy,
                ..LayoutPolicy::default()
            },
            ..Policy::default()
        };
        let fixed = |segment_size| with_layout(LayoutStrategy::Fixed { segment_size });

        assert!(fixed(1).validate().is_ok());
        assert!(fixed(MAX_SEGMENT_SIZE).validate().is_ok());
        assert_eq!(
            fixed(0).validate().unwrap_err(),
            PolicyError::SegmentSizeOutOfRange { size: 0 }
        );
        assert_eq!(
            fixed(MAX_SEGMENT_SIZE + 1).validate().unwrap_err(),
            PolicyError::SegmentSizeOutOfRange {
                size: MAX_SEGMENT_SIZE + 1
            }
        );
        assert!(Policy::default().validate().is_ok());
    }

    #[test]
    fn classical_with_dedup_is_accepted() {
        let policy = Policy {
            crypto_profile: CryptoProfile::Classical,
            dedupe: true,
            ..Policy::default()
        };
        policy.validate().expect("Classical + dedupe must validate");
    }

    #[test]
    fn test_encryption_policy() {
        let disabled = EncryptionPolicy::Disabled;
        assert!(!disabled.is_enabled());
        assert_eq!(disabled.key_version(), None);

        let enabled = EncryptionPolicy::XtsAes256 {
            key_version: Some(1),
        };
        assert!(enabled.is_enabled());
        assert_eq!(enabled.key_version(), Some(1));

        let enabled_auto = EncryptionPolicy::XtsAes256 { key_version: None };
        assert!(enabled_auto.is_enabled());
        assert_eq!(enabled_auto.key_version(), None);
    }

    #[test]
    fn test_encrypted_presets() {
        let encrypted = Policy::encrypted();
        assert!(encrypted.encryption.is_enabled());
        assert!(encrypted.dedupe);

        let encrypted_compressed = Policy::encrypted_compressed();
        assert!(encrypted_compressed.encryption.is_enabled());
        assert!(matches!(
            encrypted_compressed.compression,
            CompressionPolicy::Zstd { .. }
        ));
    }

    #[test]
    fn test_serialization() {
        let policy = Policy::default();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: Policy = serde_json::from_str(&json).unwrap();
        assert_eq!(policy.dedupe, deserialized.dedupe);
        assert_eq!(policy.crypto_profile, CryptoProfile::Classical);
    }

    #[test]
    fn test_encryption_serialization() {
        let policy = Policy::encrypted();
        let json = serde_json::to_string(&policy).unwrap();
        let deserialized: Policy = serde_json::from_str(&json).unwrap();
        assert!(deserialized.encryption.is_enabled());
    }

    #[test]
    fn test_federation_policy_deserialization_variants() {
        let shorthand_yaml = r#"
federation:
  - us-west
"#;
        let policy: Policy = serde_norway::from_str(shorthand_yaml).unwrap();
        assert_eq!(policy.federation.targets, vec!["us-west".to_string()]);
        assert_eq!(policy.federation.priority, TransferPriority::Background);

        let explicit_yaml = r#"
federation:
  targets: ["eu-central-1"]
  priority: critical
"#;
        let policy: Policy = serde_norway::from_str(explicit_yaml).unwrap();
        assert_eq!(policy.federation.targets, vec!["eu-central-1".to_string()]);
        assert_eq!(policy.federation.priority, TransferPriority::Critical);

        let legacy_json = r#"{"federation":{"target_zones":["zone-b"],"strategy":"replicate-to"}}"#;
        let policy: Policy = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(policy.federation.targets, vec!["zone-b".to_string()]);
    }

    #[test]
    fn test_transform_policy_deserialization() {
        let yaml = r#"
transform:
  - name: resize
    image: capsule://wasm-registry/resize.wasm
    trigger: on-read
    args:
      width: "1080"
    resources:
      max_memory_pages: 16
      fuel_limit: 1000
    verification:
      sha256: deadbeef
"#;

        let policy: Policy = serde_norway::from_str(yaml).unwrap();
        assert_eq!(policy.transform.len(), 1);
        assert_eq!(policy.transform[0].trigger, TransformTrigger::OnRead);
        assert_eq!(policy.transform[0].args.get("width").unwrap(), "1080");
        assert_eq!(policy.transform[0].resources.max_memory_pages, 16);
        assert_eq!(policy.transform[0].resources.fuel_limit, 1000);
        assert_eq!(
            policy.transform[0].verification.as_ref().unwrap().sha256,
            "deadbeef"
        );
    }
}
