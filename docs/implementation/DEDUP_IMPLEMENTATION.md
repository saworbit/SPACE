# Deduplication Implementation

## Overview

SPACE implements **content-addressed deduplication** at the segment level, operating on compressed data. The hash is computed pre-encryption so that XTS-AES-256 (with deterministic tweaks derived from the same content hash) preserves dedup across ciphertext — the architectural claim from `patentable_concepts.md`. Encryption is implemented and integrated into the write pipeline; see [ENCRYPTION_IMPLEMENTATION.md](ENCRYPTION_IMPLEMENTATION.md) for details.

## Architecture

### Flow Diagram

Input Data
    │
    ├─► Split into 4MB segments
    │
    ├─► Compress each segment (LZ4/Zstd, entropy-aware)
    │
    ├─► Hash compressed data (BLAKE3, algorithm-domain-separated)
    │
    ├─► Encrypt if enabled (XTS-AES-256, tweak derived from hash) + BLAKE3-MAC
    │
    ├─► Check content store (key = pre-encryption hash)
    │   ├─ Hit?  → Reuse existing segment, bump ref_count
    │   └─ Miss? → Write new segment, register hash
    │
    └─► Build capsule metadata

The dedup *key* is the hash of compressed-pre-encryption bytes; the dedup *check* runs after the encrypt step (see "Step 3: Encrypt if enabled (before dedup check)" in `crates/capsule-registry/src/pipeline/legacy.rs`). Dedup is preserved by pre-encryption hash lookup, not by ciphertext-level determinism:

- **Classical XTS path** (default `CryptoProfile`): the tweak is `derive_tweak_from_hash(content_hash)`, so identical plaintext under the same key version also produces identical ciphertext. The implementation still looks up by content hash, not ciphertext — but the ciphertext-equality property is what makes the on-disk dedup'd segment safely reusable across capsules.
- **`advanced-security` + `CryptoProfile::HybridKyber`**: ML-KEM wraps a *per-capsule/segment* XTS key (via `wrap_xts_key(profile, base, capsule_id, segment_id, content_hash)`) and the tweak is mixed with a per-segment nonce, so identical plaintext can produce *different* ciphertext across capsules. Dedup still works because the lookup key remains the pre-encryption content hash.

### Key Design Decisions

1. **Post-Compression Deduplication**
   - Hash is computed on *compressed* data, not raw data
   - Trade-off: Lower dedup ratio than pre-compression, but stable across encryption (deterministic XTS tweak is derived from this hash)

2. **BLAKE3 for Content Hashing**
   - Fast (1-2 GB/s single-threaded)
   - Cryptographically secure — also used to derive the XTS tweak via `derive_tweak_from_hash` in `crates/encryption/src/xts.rs`
   - 32-byte hash = 64 hex characters

3. **Content Store Design**
   - `ContentHash → SegmentId` map persisted in the registry
   - No bloom filter yet (potential optimization for very large stores)

4. **Reference Counting**
   - Each segment tracks `ref_count`, updated on every dedup hit and capsule deletion
   - Drives garbage collection (see Phase 3.2 GC section below)
   - Capsules track `deduped_bytes` for monitoring
5. **Zero-Copy Compression Fast-Path**
   - Compression returns `Cow<[u8]>`, borrowing the original slice when compression is skipped
   - Hashing, dedup lookups, and optional encryption operate directly on the borrowed buffer
   - Only segments that actually compress or encrypt allocate new `Vec<u8>`
6. **Transactional Staging for Async Writes**
   - When the `pipeline_async` feature is enabled, `WritePipeline` saturates Tokio workers and stages new segment data inside `NvramTransaction`
   - Dedup hits are handled in two phases: staged reuse (within the same capsule) updates pending segment refcounts, while persistent reuse increments existing segments lazily and is rolled back if the transaction aborts
   - Content-store registration is deferred until the transaction successfully commits, guaranteeing that hashes only point at durable on-disk segments
7. **Algorithm-domain-separated dedup key**
   - The dedup index must guarantee `key(a) == key(b) ⇒ read(a) == read(b)`. Bare `hash_content(data)` violates this when two segments share stored bytes but require different decompression — e.g. an LZ4 frame stored raw under `CompressionPolicy::None` vs the plaintext compressed under `CompressionPolicy::LZ4`
   - Write paths use `hash_content_with_algo(data, comp_result.algorithm)`, which mixes the algorithm name into a versioned BLAKE3 prefix: `b"space.dedup.v1\0algo:" || algo || b"\0" || data`. `Deduper::hash_content_with_algo` is a required trait method (no defaulted shim) so new implementations cannot silently reintroduce the collision
   - Scrub verification calls `dedup::verify_content_hash(expected, data, algo)`, which returns `Matched`, `LegacyMatched` (pre-fix bare-hash compatibility window — counted in `ScrubReport::legacy_hash_hits`), or `Mismatched`. The write index never accepts the legacy form

## Files Modified

### Core Implementation

| File | Changes | Purpose |
|------|---------|---------|
| `common/src/lib.rs` | Added `ContentHash`, `deduped_bytes`, `ref_count` | Type definitions |
| `capsule-registry/src/dedup.rs` | NEW | Hashing logic + stats |
| `capsule-registry/src/lib.rs` | Added `content_store`, lookup/register methods | Content-addressed storage |
| `capsule-registry/src/pipeline.rs` | Integrated dedup into write path | Main logic |
| `nvram-sim/src/lib.rs` | Updated Segment initialization | Metadata support |

### Dependencies

| Crate | Version | Purpose |
|-------|---------|---------|
| `blake3` | 1.5 | Content hashing |
| `hex` | 0.4 | Hash encoding |

## API Usage

### Enable/Disable Deduplication

// Default policy has dedup enabled
let policy = Policy::default();
assert!(policy.dedupe);

// Disable dedup for pre-compressed data
let policy = Policy::precompressed();
assert!(!policy.dedupe);

### Write with Deduplication

let registry = CapsuleRegistry::new();
let nvram = NvramLog::open("space.nvram")?;
let pipeline = WritePipeline::new(registry, nvram);

// Automatic deduplication
let capsule_id = pipeline.write_capsule(data)?;

// Output shows dedup hits:
// ♻️  Dedup hit: Reusing segment 5 (saved 4194304 bytes)

### Check Dedup Statistics

let (total_segments, unique_segments) = registry.get_dedup_stats();
let dedup_ratio = total_segments as f32 / unique_segments as f32;

println!("Deduplication ratio: {:.2}x", dedup_ratio);

## Performance Characteristics

### Time Complexity

| Operation | Complexity | Notes |
|-----------|-----------|-------|
| Hash computation | O(n) | n = segment size (4MB), ~2ms @ 2GB/s |
| Content lookup | O(1) | HashMap lookup |
| Segment write | O(1) | Append-only log |

### Space Savings

**Test Results** (from `dedup_test.rs`):

| Scenario | Dedup Ratio | Notes |
|----------|-------------|-------|
| 3 identical 6MB capsules | ~3.0x | Perfect dedup |
| Repeated patterns | 1.5-2.5x | Segment-level granularity |
| Random data | 1.0x | No dedup (expected) |

### Overhead

- Hash computation: ~2ms per 4MB segment (negligible)
- Metadata overhead: 64 bytes per unique segment (hash)
- Memory: Content store scales with unique segment count
- Allocations: Zero-copy path avoids cloning for entropy-skipped segments, improving large transfer latency by ~10-20% in internal profiling

## Testing

### Unit Tests

# Test content hashing
cargo test -p capsule-registry hash_content

# Test dedup stats tracking
cargo test -p capsule-registry dedup_stats

### Integration Tests

# Full dedup test suite
cargo test --test dedup_test -- --nocapture

# Specific scenarios
cargo test --test dedup_test test_dedup_identical_segments
cargo test --test dedup_test test_dedup_multiple_capsules

### Manual Testing

# Run the dedup demo script
chmod +x scripts/test_dedup.sh
./scripts/test_dedup.sh

## Metadata Format

### Content Store (in `space.db`)

{
  "content_store": {
    "a1b2c3...": 42,
    "d4e5f6...": 43
  },
  "capsules": {
    "550e8400-...": {
      "segments": [42, 42, 43],
      "deduped_bytes": 4194304
    }
  }
}

**Interpretation:**
- Segments 42 used twice (deduped once)
- Capsule saved 4MB via deduplication

## Phase 3.2: Garbage Collection Implementation

- **Reference-counted segments**: `Segment.ref_count` is updated on every dedup hit and capsule deletion, ensuring shared segments stay consistent across capsules.
- **Startup reconciliation**: `WritePipeline::new` rebuilds refcounts from persisted capsule metadata, fixing drift after crashes or manual edits.
- **Garbage collector**: `gc::GarbageCollector` walks `NvramLog::list_segments()` and removes segments whose refcount hit zero, pruning both metadata and content-store entries.
- **Deletion path**: `WritePipeline::delete_capsule` decrements segment refcounts and drops orphaned hashes immediately, keeping capsules and segments in sync.
- **Regression tests**: `gc_test.rs` covers multi-capsule refcounts and orphan sweeping to guard against regressions.

## Known Limitations

1. **No Cross-Node Dedup**
   - Content store is local per node
   - Federation/clustering across PODMS mesh is experimental

2. **No Bloom Filter**
   - Content store grows with unique segments
   - May need optimization for very large stores (1M+ segments)

3. **Fixed Segment Granularity**
   - Small duplicates below segment size are not detected
   - Variable-length (rolling-hash) dedup remains a future optimization

## Future Enhancements

- [ ] Distributed content store across PODMS mesh nodes
- [ ] Variable-length deduplication (rolling hash)
- [ ] Bloom filter for negative lookups at scale
- [ ] GPU-accelerated bloom filter (as per patent doc)

## Validation Against Patent Claims

From `../patentable_concepts.md` § 3:

> **Per-Segment Encryption with Inline Dedup & Compression**
>
> Encrypt XTS-AES-256 per 256 MiB segment *after* compression + dedupe yet retain global dedupe across ciphertext via deterministic IV derivation.

**Current status:**

✅ Compression before dedup: **IMPLEMENTED**
✅ Content-addressed storage: **IMPLEMENTED**
✅ Hash-based dedup: **IMPLEMENTED**
✅ Encryption with deterministic tweak (classical XTS-AES-256 path, tweak from BLAKE3 content hash): **IMPLEMENTED**
✅ Global dedupe across encrypted data via pre-encryption hash lookup: **IMPLEMENTED**
✅ Algorithm-domain-separated dedup key (`hash_content_with_algo`): **IMPLEMENTED**
✅ Reference-counted segments with GC: **IMPLEMENTED**

Caveat on "deterministic ciphertext for identical plaintext": this holds for the classical XTS path only. Under `advanced-security` + `CryptoProfile::HybridKyber`, the per-capsule/segment ML-KEM key wrap and nonce-mixed tweak break ciphertext-level determinism; dedup is preserved in that path because the lookup key is the pre-encryption content hash, not the ciphertext.

The single-node dedup-over-encrypted-data claim is realized end-to-end. Cross-node dedupe over the PODMS mesh remains experimental.

## Troubleshooting

### Dedup Not Occurring

**Symptom:** All segments unique despite identical data

**Causes:**
1. Different compression algorithms between writes
2. Dedup disabled in policy (`policy.dedupe = false`)
3. Data genuinely unique (check entropy)

**Debug:**

- Use `cargo test -p capsule-registry dedup_test -- --nocapture` to print dedup stats (sled data files are binary and not human-readable).
- Enable verbose logging: `RUST_LOG=debug cargo run -- create --file test.txt`

### High Memory Usage

**Symptom:** Memory grows with capsule count

**Cause:** Content store keeps all hashes in memory

**Solution (Phase 3):**
- Implement bloom filter
- Offload to external KV store (FoundationDB)

## Performance Benchmarks

Run the benchmark suite:

cargo bench --bench dedup_bench

**Expected Results:**
- Hash computation: ~2ms per 4MB segment
- Content lookup: <1μs (HashMap)
- Dedup overhead: <1% of total write time

## Summary

Content-addressed deduplication is implemented and integrated end-to-end with the encryption pipeline:

- ✅ Deduplicates at segment granularity
- ✅ Operates on compressed data (not plaintext)
- ✅ Uses cryptographic hashing (BLAKE3), algorithm-domain-separated to prevent cross-policy collisions
- ✅ Preserves data integrity across all test scenarios
- ✅ Composes with XTS-AES-256 encryption: dedup is preserved end-to-end by pre-encryption hash lookup. The classical `CryptoProfile` path additionally produces deterministic ciphertext (tweak derived from the content hash); the `advanced-security` + `HybridKyber` path mixes per-capsule/segment material and so does *not* produce identical ciphertext, but dedup still works because lookup is hash-based
- ✅ Reference-counted with garbage collection
- ✅ Maintains performance (<1% overhead from dedup itself)
- ✅ Scales to thousands of segments

The single-node "dedupe over ciphertext" concept from the patent documentation is fully realized. Cross-node dedup over the PODMS mesh remains experimental.
