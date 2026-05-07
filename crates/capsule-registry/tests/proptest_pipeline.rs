//! Property-based tests for the SPACE write/read pipeline.
//!
//! Covers the invariants v0.2 ("Core Capsule") stakes everything on:
//!
//! 1. **Round-trip correctness** — `read(write(bytes)) == bytes` for arbitrary
//!    payloads, with and without encryption, across segment boundaries.
//! 2. **Dedup determinism** — writing identical content N times reuses the
//!    same segment IDs and bumps `ref_count` to N.
//! 3. **Content separation** — distinct payloads produce distinct segment
//!    sets (no false dedup hits).
//! 4. **Boundary sizes** — empty input, single byte, around the 4 MiB
//!    segment boundary all round-trip cleanly.
//!
//! Tests share a single Tokio runtime across proptest cases; each case opens
//! a fresh `NvramLog` in a unique temp file so cases stay isolated.

use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;

use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use common::Policy;
use encryption::keymanager::{KeyManager, MASTER_KEY_SIZE};
use nvram_sim::NvramLog;
use proptest::prelude::*;
use tokio::runtime::Runtime;
use uuid::Uuid;

fn runtime() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| Runtime::new().expect("failed to build tokio runtime for proptests"))
}

struct PipelineHarness {
    pipeline: WritePipeline,
    registry: CapsuleRegistry,
    nvram: NvramLog,
    log_path: PathBuf,
    meta_path: PathBuf,
}

impl PipelineHarness {
    fn new_unencrypted() -> Self {
        let (log_path, meta_path) = unique_paths("proptest");
        let registry =
            CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).expect("open registry");
        let nvram = NvramLog::open(log_path.to_string_lossy().as_ref()).expect("open nvram");
        let pipeline = WritePipeline::new(registry.clone(), nvram.clone());
        Self {
            pipeline,
            registry,
            nvram,
            log_path,
            meta_path,
        }
    }

    fn new_encrypted() -> Self {
        let (log_path, meta_path) = unique_paths("proptest_enc");
        let registry =
            CapsuleRegistry::open(meta_path.to_string_lossy().as_ref()).expect("open registry");
        let nvram = NvramLog::open(log_path.to_string_lossy().as_ref()).expect("open nvram");
        // Deterministic test key — the property under test is round-trip
        // correctness, not key secrecy.
        let key_manager = KeyManager::new([0xA5u8; MASTER_KEY_SIZE]);
        let pipeline =
            WritePipeline::with_key_manager(registry.clone(), nvram.clone(), key_manager);
        Self {
            pipeline,
            registry,
            nvram,
            log_path,
            meta_path,
        }
    }
}

impl Drop for PipelineHarness {
    fn drop(&mut self) {
        // Best-effort cleanup; tests should not fail if a file is gone already.
        let _ = fs::remove_file(&self.log_path);
        let _ = fs::remove_file(format!("{}.segments", self.log_path.to_string_lossy()));
        let _ = fs::remove_file(&self.meta_path);
    }
}

fn unique_paths(prefix: &str) -> (PathBuf, PathBuf) {
    let base = std::env::temp_dir().join("space_proptest_pipeline");
    let _ = fs::create_dir_all(&base);
    let unique = Uuid::new_v4();
    (
        base.join(format!("{prefix}_{unique}.log")),
        base.join(format!("{prefix}_{unique}.metadata")),
    )
}

// ---------------------------------------------------------------------------
// Property: round-trip correctness without encryption
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        // Each case touches the filesystem, so keep counts modest. The set
        // is still varied enough to find regressions in compression/dedup
        // glue without taking minutes to run.
        cases: 32,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_roundtrip_unencrypted(
        payload in prop::collection::vec(any::<u8>(), 0..16_384usize),
    ) {
        runtime().block_on(async {
            let harness = PipelineHarness::new_unencrypted();
            let id = harness
                .pipeline
                .write_capsule(&payload)
                .await
                .expect("write");
            let recovered = harness
                .pipeline
                .read_capsule(id)
                .await
                .expect("read");
            prop_assert_eq!(payload, recovered);
            Ok(())
        })?;
    }

    // Note: XTS-AES requires at least one full cipher block (16 bytes) of
    // input. Plaintexts shorter than that are rejected by the encryption
    // path with "Invalid ciphertext length: N". Handling sub-block payloads
    // (via padding, GCM fallback, or a small-blob bypass) is a separate
    // pipeline change and out of scope for these tests; the property here
    // covers the supported size range only.
    #[test]
    fn prop_roundtrip_encrypted(
        payload in prop::collection::vec(any::<u8>(), 16..16_384usize),
    ) {
        runtime().block_on(async {
            let harness = PipelineHarness::new_encrypted();
            let policy = Policy::encrypted();
            let id = harness
                .pipeline
                .write_capsule_with_policy(&payload, &policy)
                .await
                .expect("write");
            let recovered = harness
                .pipeline
                .read_capsule(id)
                .await
                .expect("read");
            prop_assert_eq!(payload, recovered);

            // Sanity: at least one stored segment is marked encrypted.
            let capsule = harness.registry.lookup(id).expect("lookup");
            prop_assert!(!capsule.segments.is_empty());
            let seg = harness
                .nvram
                .get_segment_metadata(capsule.segments[0])
                .expect("segment metadata");
            prop_assert!(
                seg.encrypted,
                "segment should be flagged encrypted under Policy::encrypted()"
            );
            Ok(())
        })?;
    }
}

// ---------------------------------------------------------------------------
// Property: dedup is deterministic — same input N times shares segments
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 24,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_dedup_same_payload_shares_segments(
        // Use a moderate size so we get at least one full segment to share.
        payload in prop::collection::vec(any::<u8>(), 1024..32_768usize),
        writes in 2u32..6,
    ) {
        runtime().block_on(async {
            let harness = PipelineHarness::new_unencrypted();
            let policy = Policy::default();

            let mut ids = Vec::with_capacity(writes as usize);
            for _ in 0..writes {
                let id = harness
                    .pipeline
                    .write_capsule_with_policy(&payload, &policy)
                    .await
                    .expect("write");
                ids.push(id);
            }

            // All capsules must reference the same segment list — that's the
            // observable proof that dedup fired on every subsequent write.
            let first = harness.registry.lookup(ids[0]).expect("lookup first");
            prop_assert!(!first.segments.is_empty());
            for id in &ids[1..] {
                let other = harness.registry.lookup(*id).expect("lookup other");
                prop_assert_eq!(
                    &first.segments,
                    &other.segments,
                    "dedup must produce identical segment lists for identical payloads"
                );
            }

            // Each shared segment's ref_count should equal the number of writes.
            for seg_id in &first.segments {
                let seg = harness
                    .nvram
                    .get_segment_metadata(*seg_id)
                    .expect("segment metadata");
                prop_assert_eq!(
                    seg.ref_count,
                    writes,
                    "ref_count must match number of capsules referencing the segment"
                );
                if writes >= 2 {
                    prop_assert!(
                        seg.deduplicated,
                        "segment shared by 2+ capsules must be flagged deduplicated"
                    );
                }
            }
            Ok(())
        })?;
    }

    #[test]
    fn prop_distinct_payloads_distinct_segments(
        a in prop::collection::vec(any::<u8>(), 1024..16_384usize),
        b in prop::collection::vec(any::<u8>(), 1024..16_384usize),
    ) {
        // Skip the rare case where the random vectors collide.
        prop_assume!(a != b);

        runtime().block_on(async {
            let harness = PipelineHarness::new_unencrypted();
            let policy = Policy::default();

            let id_a = harness
                .pipeline
                .write_capsule_with_policy(&a, &policy)
                .await
                .expect("write a");
            let id_b = harness
                .pipeline
                .write_capsule_with_policy(&b, &policy)
                .await
                .expect("write b");

            let cap_a = harness.registry.lookup(id_a).expect("lookup a");
            let cap_b = harness.registry.lookup(id_b).expect("lookup b");

            // Distinct content with sufficient size must not produce the
            // exact same segment list — that would indicate a hash collision
            // (astronomically unlikely with BLAKE3) or a dedup bug.
            prop_assert_ne!(
                &cap_a.segments,
                &cap_b.segments,
                "distinct payloads must not produce identical segment lists"
            );

            // Both round-trip cleanly.
            let read_a = harness.pipeline.read_capsule(id_a).await.expect("read a");
            let read_b = harness.pipeline.read_capsule(id_b).await.expect("read b");
            prop_assert_eq!(a, read_a);
            prop_assert_eq!(b, read_b);
            Ok(())
        })?;
    }
}

// ---------------------------------------------------------------------------
// Property: dedup must not collide across compression policies
//
// Regression for the cross-policy dedup-key bug: hashing only the stored
// bytes (without compression-algorithm context) lets two writes with the
// same on-disk bytes but different decompression treatments share a segment,
// after which the second reader gets the wrong plaintext.
//
// Construction: take an arbitrary plaintext P, compress it with LZ4 to get
// frame F, then write two capsules:
//   1. Capsule A: write F under `CompressionPolicy::None` → segment stored
//      raw with bytes = F, `compressed = false`.
//   2. Capsule B: write P under `CompressionPolicy::LZ4` → adaptive
//      compressor produces (likely) F, segment stored with `compressed = true`.
//
// Both reads must round-trip to their original payloads. With the dedup key
// not domain-separated by compression algo, capsule B can dedup onto
// capsule A's segment and read back F instead of P.
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(ProptestConfig {
        cases: 16,
        .. ProptestConfig::default()
    })]

    #[test]
    fn prop_no_cross_policy_dedup_collision(
        // Compressible plaintext (repeating pattern) so LZ4 actually compresses.
        // Size kept moderate to keep the test fast.
        plaintext in prop::collection::vec(any::<u8>(), 256..4_096usize)
            .prop_map(|seed| {
                // Tile the seed to make the plaintext compressible.
                let mut tiled = Vec::with_capacity(seed.len() * 8);
                for _ in 0..8 { tiled.extend_from_slice(&seed); }
                tiled
            }),
    ) {
        runtime().block_on(async {
            // Compress plaintext with the default LZ4 policy outside the
            // pipeline so we have the raw frame bytes to write under None.
            let lz4_policy = common::policy::CompressionPolicy::default();
            // If default is not LZ4, force LZ4 explicitly.
            let lz4_policy = match lz4_policy {
                common::policy::CompressionPolicy::LZ4 { .. } => lz4_policy,
                _ => common::policy::CompressionPolicy::LZ4 { level: 1 },
            };
            let (frame_view, _summary) =
                ::compression::compress_segment(&plaintext, &lz4_policy)
                    .expect("compress plaintext");
            let frame = frame_view.into_owned();

            // Skip cases where adaptive compression decided not to compress
            // (output >= input). The collision scenario only arises when
            // capsule B's segment ends up with `compressed = true`.
            prop_assume!(frame != plaintext);

            let harness = PipelineHarness::new_unencrypted();

            // Capsule A: write the LZ4 frame as raw bytes (None policy).
            let none_policy = Policy {
                compression: common::policy::CompressionPolicy::None,
                ..Policy::default()
            };
            let id_a = harness
                .pipeline
                .write_capsule_with_policy(&frame, &none_policy)
                .await
                .expect("write A");

            // Capsule B: write the plaintext with LZ4 enabled.
            let lz4_capsule_policy = Policy {
                compression: lz4_policy,
                ..Policy::default()
            };
            let id_b = harness
                .pipeline
                .write_capsule_with_policy(&plaintext, &lz4_capsule_policy)
                .await
                .expect("write B");

            // Both reads must return their original payloads — no collision.
            let read_a = harness.pipeline.read_capsule(id_a).await.expect("read A");
            let read_b = harness.pipeline.read_capsule(id_b).await.expect("read B");

            prop_assert_eq!(
                &frame, &read_a,
                "capsule A (raw LZ4 frame under None policy) must round-trip to its frame bytes"
            );
            prop_assert_eq!(
                &plaintext, &read_b,
                "capsule B (plaintext under LZ4 policy) must round-trip to its plaintext, \
                 not the LZ4 frame from capsule A — dedup key must include compression algo"
            );

            // And the segment lists must be disjoint: cross-policy dedup is
            // forbidden, so A and B should not share any segment IDs.
            let cap_a = harness.registry.lookup(id_a).expect("lookup A");
            let cap_b = harness.registry.lookup(id_b).expect("lookup B");
            for seg in &cap_a.segments {
                prop_assert!(
                    !cap_b.segments.contains(seg),
                    "capsules with different compression policies must not share segments"
                );
            }
            Ok(())
        })?;
    }
}

// ---------------------------------------------------------------------------
// Boundary cases — pinned (not randomized) so regressions get a deterministic
// signal and these cover sizes proptest is unlikely to hit by chance.
// ---------------------------------------------------------------------------

const FOUR_MIB: usize = 4 * 1024 * 1024;

#[tokio::test]
async fn boundary_empty_payload_roundtrips() {
    let harness = PipelineHarness::new_unencrypted();
    let id = harness.pipeline.write_capsule(&[]).await.expect("write");
    let recovered = harness.pipeline.read_capsule(id).await.expect("read");
    assert!(recovered.is_empty());
}

#[tokio::test]
async fn boundary_single_byte_roundtrips() {
    let harness = PipelineHarness::new_unencrypted();
    let id = harness
        .pipeline
        .write_capsule(&[0x42])
        .await
        .expect("write");
    let recovered = harness.pipeline.read_capsule(id).await.expect("read");
    assert_eq!(recovered, vec![0x42]);
}

#[tokio::test]
async fn boundary_segment_size_minus_one_roundtrips() {
    let harness = PipelineHarness::new_unencrypted();
    let payload: Vec<u8> = (0..FOUR_MIB - 1).map(|i| (i & 0xFF) as u8).collect();
    let id = harness
        .pipeline
        .write_capsule(&payload)
        .await
        .expect("write");
    let recovered = harness.pipeline.read_capsule(id).await.expect("read");
    assert_eq!(recovered, payload);
}

#[tokio::test]
async fn boundary_segment_size_exact_roundtrips() {
    let harness = PipelineHarness::new_unencrypted();
    let payload: Vec<u8> = (0..FOUR_MIB).map(|i| (i & 0xFF) as u8).collect();
    let id = harness
        .pipeline
        .write_capsule(&payload)
        .await
        .expect("write");
    let recovered = harness.pipeline.read_capsule(id).await.expect("read");
    assert_eq!(recovered, payload);
}

#[tokio::test]
async fn boundary_segment_size_plus_one_crosses_segment_boundary() {
    let harness = PipelineHarness::new_unencrypted();
    let payload: Vec<u8> = (0..FOUR_MIB + 1).map(|i| (i & 0xFF) as u8).collect();
    let id = harness
        .pipeline
        .write_capsule(&payload)
        .await
        .expect("write");
    let recovered = harness.pipeline.read_capsule(id).await.expect("read");
    assert_eq!(recovered, payload);

    // A payload one byte over the segment size should produce 2 segments.
    let capsule = harness.registry.lookup(id).expect("lookup");
    assert_eq!(
        capsule.segments.len(),
        2,
        "4 MiB + 1 byte should produce exactly two segments"
    );
}
