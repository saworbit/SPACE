# The Capsule

> The foundational unit of storage in SPACE. Everything else — protocols, replication, federation, scrub, tiering — is downstream of this object. Read this document first.

---

## Table of Contents

1. [Why the Capsule exists](#1-why-the-capsule-exists)
2. [What a Capsule is](#2-what-a-capsule-is)
3. [The data model](#3-the-data-model)
4. [Where Capsules live](#4-where-capsules-live)
5. [The write flow](#5-the-write-flow)
6. [The read flow](#6-the-read-flow)
7. [Deletion, ref counting, and GC](#7-deletion-ref-counting-and-gc)
8. [Scrub: continuous integrity](#8-scrub-continuous-integrity)
9. [Architecture: where each responsibility lives](#9-architecture-where-each-responsibility-lives)
10. [Design rationale](#10-design-rationale)
11. [Invariants](#11-invariants)
12. [Failure modes and recovery](#12-failure-modes-and-recovery)
13. [Glossary](#13-glossary)
14. [Cross-references](#14-cross-references)

---

## 1. Why the Capsule exists

### The problem

Conventional storage stacks bind **data shape to protocol shape**. A file lives in a filesystem; a block in a LUN; an object in a bucket. Moving data across protocols means copying it, re-encoding it, and re-securing it — losing dedup, losing locality, losing keys. Adding a new protocol means adding a new storage path.

This is a 30-year-old assumption baked into nearly every storage product. It fails badly for modern workloads that mix files, objects, blocks, and streams against the same underlying bytes.

### The bet

SPACE makes a different bet: **separate the durable representation of data from the protocol you use to talk to it**. The durable representation is the Capsule. Protocols (S3, NFS, block, NVMe-oF, FUSE, CSI) are *views* projected on top of capsules — none of them own bytes.

This means:

- The same bytes are simultaneously a file, a block, and an object — no copies.
- Dedup, compression, and encryption are properties of the capsule, not the protocol.
- Encryption keys, integrity tags, and content hashes survive protocol changes — you don't re-encrypt to switch from S3 to NFS.
- Adding a protocol is writing an adapter, not building a storage stack.

### The cost

The bet is non-trivial:

- **Every byte must fit a single, opinionated representation.** Compression, encryption, dedup, and integrity verification all assume capsule structure. You cannot opt out of the abstraction for "fast path" data.
- **Encryption must be deterministic to preserve dedup.** SPACE uses XTS-AES-256 with content-hash-derived tweaks. This is unusual — most systems pick deterministic-or-secure, not both. (See [§10.2](#102-deterministic-encryption-preserves-dedup).)
- **Metadata is on the hot path.** Every read touches Sled (for capsule lookup), the NVRAM segment map (for offsets), and the key manager (for decryption). Metadata cannot be slow.

The rest of this document explains how SPACE pays that cost.

---

## 2. What a Capsule is

A **Capsule** is a content-addressed, policy-bound collection of segments with a stable 128-bit identity.

- **Stable identity**: `CapsuleId(Uuid)` is permanent for the Capsule's lifetime. It survives re-encryption, key rotation, compression transcoding, migration between zones, replication, and protocol projection. Generated without coordination.
- **Immutable logical content**: what `read(capsule_id)` returns is preserved across every storage-layer transformation. The bytes a client sees do not change because the Capsule moved zones or rotated keys.
- **Mutable representation**: the segment list, per-segment content hashes, encryption metadata, and the `Policy` field itself **can change in place** when the Capsule is transformed by PODMS swarm migration, key rotation, or compression transcoding. See [§2.1](#21-identity-vs-representation) below.
- **Content-addressed**: every segment carries a `content_hash` (BLAKE3 over compressed-pre-encryption bytes) that uniquely identifies its payload across the cluster.
- **Policy-bound**: every Capsule carries a `Policy` describing how it is currently compressed, encrypted, replicated, and laid out. The policy is the active representation, not the historical one — transformations update it.

A Capsule is *not*:

- A file. Files are projected onto Capsules via the NFS or FUSE view.
- A block volume. Volumes are projected onto Capsules via the block view.
- An object. Objects are projected onto Capsules via the S3 view.
- A byte stream. Capsules contain bytes; they are not themselves byte streams.

The Capsule is the only logical durable object in SPACE. Its physical realization is split across two stores — segment bytes in the NVRAM log, capsule records and content-hash index in Sled — but at the abstraction layer there is only the Capsule. See [§4](#4-where-capsules-live) for the physical layout.

### 2.1 Identity vs. representation

The single rule that resolves every "is this still the same Capsule?" question:

| Operation | Origin | CapsuleId | Logical content | Representation |
|---|---|---|---|---|
| Write a new Capsule | Client | new | new | new |
| Read a Capsule | Client | unchanged | unchanged | unchanged |
| Delete a Capsule | Client | gone | gone | gone |
| Overwrite a file (NFS/FUSE/block/S3 view) | Protocol layer | **new** | new | new |
| Migrate a Capsule between zones | PODMS / data motion | **same** | same | rewritten (new encryption, possibly new compression) |
| Rotate the encryption key | Key manager | **same** | same | re-encrypted segments |
| Transcode compression (e.g. cold-tier to Zstd) | Tiering / PODMS | **same** | same | re-compressed segments, new `content_hash` per segment |
| Replicate to another node | Replication | **same** | same | same on origin; copy on destination |

The boundary is **what initiated the change**:

- **Protocol-layer mutations** (a client overwriting a file at `/foo.txt`, rewriting block offset 4096 of `vol0`, PUT-ing a new object body to `s3://bucket/key`) produce a **new** Capsule with a new `CapsuleId`. The view's metadata is updated to point at the new Capsule; the old one is dereferenced and may be GC'd. The Capsule abstraction is *not* in-place mutable from the protocol surface.
- **Storage-layer transformations** (PODMS swarm migration, key rotation, compression transcoding, tier promotion/demotion) **preserve** `CapsuleId` and logical content while rewriting the on-disk representation. Audit log continuity is preserved; references at the view layer remain valid.

This is what makes federated migration tractable: you can move a Capsule from `us-west` to `eu-central` (with re-encryption under the destination's key) without touching any file path, S3 key, or block volume that points at it.

The implementation lives in `crates/scaling/src/agent.rs::data_motion_task` — `capsule_id` is passed through verbatim from source to destination, carried in each `ReplicationFrame`, and used by the receiver in `crates/scaling/src/replication.rs` to derive the wrapped-key context. (The receiver does not persist `capsule_id` in per-segment metadata; segments stay capsule-agnostic, and the Capsule record itself owns the identity.)

---

## 3. The data model

The authoritative definitions live in [crates/common/src/lib.rs](../crates/common/src/lib.rs). This section explains them.

### 3.1 `CapsuleId`

```rust
pub struct CapsuleId(pub Uuid);
```

A 128-bit UUID v4. Generated with `CapsuleId::new()` (random) or `CapsuleId::from_uuid(u)` (lifted from an external system).

**Why 128 bits?** Wide enough to be globally unique without coordination across the entire cluster (and across all clusters that will ever exist) — necessary because Capsules can migrate between zones, be replicated, and be projected through multiple protocols. Anything narrower forces a coordinator on the write path.

`CapsuleId::shard_keys(count)` derives deterministic shard keys for distributing metadata across registry shards. Same UUID always maps to the same shards — required for read locality across replicas.

### 3.2 `SegmentId`

```rust
pub struct SegmentId(pub u64);
```

A 64-bit sequence number, monotonically increasing within an NVRAM log. Segments are append-only; their IDs never repeat within a backend. Different backends have independent sequence spaces.

**Why 64 bits?** A single NVRAM log will not exceed 2⁶⁴ segments in any plausible deployment. Wider would waste metadata; narrower would force ID recycling and complicate GC.

### 3.3 `ContentHash`

```rust
pub struct ContentHash(pub String);
```

A hex-encoded BLAKE3 hash of a segment's **compressed, pre-encryption** bytes, with the compression algorithm mixed in for domain separation.

```rust
// crates/dedup/src/lib.rs
pub fn hash_content_with_algo(data: &[u8], algo: &str) -> ContentHash {
    let mut hasher = blake3::Hasher::new();
    hasher.update(b"space.dedup.v1\0algo:");
    hasher.update(algo.as_bytes());
    hasher.update(b"\0");
    hasher.update(data);
    ContentHash::from_bytes(hasher.finalize().as_bytes())
}
```

The domain prefix and algorithm separator are critical — see [§10.4](#104-dedup-keys-need-algorithm-domain-separation).

### 3.4 `Capsule`

```rust
pub struct Capsule {
    pub id: CapsuleId,
    pub size: u64,
    pub segments: Vec<SegmentId>,
    pub created_at: u64,
    pub policy: Policy,
    pub deduped_bytes: u64,
}
```

| Field | Purpose | Rationale |
|---|---|---|
| `id` | Unique identity | See §3.1 |
| `size` | Logical (uncompressed, decrypted) byte count | Required for range reads without materializing segments. Allows reporting `df` / `ls -l` semantics through views. |
| `segments` | Ordered list of segment IDs | Reads walk this list. Order = logical byte order. |
| `created_at` | Unix seconds at creation | Used by heat/age policies and audit. |
| `policy` | The compression/encryption/layout/federation/transform policy **currently active**. Updated in place by storage-layer transformations (PODMS migration, key rotation, transcoding). | Read-path code must NOT branch on this — branch on segment metadata. See [§2.1](#21-identity-vs-representation) and [§3.6](#36-policy). |
| `deduped_bytes` | Bytes that hit existing segments during write | Operator visibility into dedup effectiveness per Capsule. |

### 3.5 `Segment`

```rust
pub struct Segment {
    pub id: SegmentId,
    pub offset: u64,
    pub len: u32,
    pub plain_len: Option<u32>,
    pub compressed: bool,
    pub compression_algo: String,
    pub content_hash: Option<ContentHash>,
    pub ref_count: u32,
    pub deduplicated: bool,
    pub access_count: u32,
    pub encryption_version: Option<u16>,
    pub key_version: Option<u32>,
    pub tweak_nonce: Option<[u8; 16]>,
    pub integrity_tag: Option<[u8; 16]>,
    pub encrypted: bool,
    pub pq_ciphertext: Option<String>,
    pub pq_nonce: Option<[u8; 16]>,
}
```

| Field | Set by | Read by |
|---|---|---|
| `id`, `offset`, `len` | Append to backend log | Reader (where to seek, how much to read) |
| `plain_len` | Compressor (logical size before compression) | Range-read planner (skip segments outside range without decompressing) |
| `compressed`, `compression_algo` | Write pipeline | Reader (choose decompressor); scrub (verify content_hash with same algo) |
| `content_hash` | Dedup stage, pre-encryption | Dedup lookup; scrub (unencrypted deep verification) |
| `ref_count` | Inc on dedup hit / new capsule; dec on delete | GC sweep (reclaim when 0) |
| `deduplicated` | Write pipeline (true if any capsule shares this segment) | Operator visibility |
| `access_count` | Reader (counters) | Tiering heatmap |
| `encryption_version`, `key_version`, `tweak_nonce`, `integrity_tag`, `encrypted` | Encryption stage | Reader (key selection, MAC verify); scrub (deep MAC check) |
| `pq_ciphertext`, `pq_nonce` | Post-quantum hybrid wrap (gated) | Hybrid decrypt path |

> **Invariant: trust segment metadata, not policy, on read.** Branch on `segment.compressed` and `segment.encrypted`, never on `capsule.policy`. The policy reflects the *current* representation, but segments can be heterogeneous during transformation; segment fields are the durable truth. See [§11](#11-invariants).

### 3.6 `Policy`

Lives in [crates/common/src/policy.rs](../crates/common/src/policy.rs). High-level shape:

```rust
pub struct Policy {
    pub compression: CompressionPolicy,        // None | LZ4{level} | Zstd{level}
    pub dedupe: bool,
    pub compact_interval_secs: Option<u64>,
    pub erasure_profile: Option<String>,
    pub encryption: EncryptionPolicy,          // Disabled | XtsAes256{key_version}
    pub crypto_profile: CryptoProfile,         // Classical | HybridKyber
    pub layout: LayoutPolicy,                  // strategy + EC profile + heat threshold
    pub federation: FederationPolicy,          // target zones, priority, strategy
    pub transform: Vec<TransformDef>,          // WASM transforms (OnRead/OnWrite)
    // PODMS-gated:
    pub rpo: Duration,
    pub latency_target: Duration,
    pub sovereignty: SovereigntyLevel,
    pub replica_count: u8,
}
```

Presets (`Policy::default()`, `text_optimized()`, `precompressed()`, `edge_optimized()`, `encrypted()`, `encrypted_compressed()`, `metro_sync()`, `geo_replicated()`) provide named bundles for common workloads. Custom policies are constructed field-by-field.

### 3.7 `Event`

Audit-log events emitted by the registry: `CapsuleCreated`, `CapsuleRead`, `CapsuleDeleted`, `SegmentAppended`, `DedupHit`, `AuditHeartbeat`. These are the immutable record of what happened to a Capsule — used by the security model and replication.

---

## 4. Where Capsules live

### 4.1 The two-store model

A Capsule's existence is split across two persistence layers:

| Store | Holds | Format | Crate |
|---|---|---|---|
| **NVRAM log** | Segment bytes + segment metadata | Append-only log + JSON sidecar | [`nvram-sim`](../crates/nvram-sim/) |
| **Capsule registry** | Capsule records + content hash index | Sled (embedded KV) | [`capsule-registry`](../crates/capsule-registry/) |

The split is deliberate: segment bytes are bulky and want streaming I/O; capsule metadata is small and wants point lookups + transactions. Fusing them would mean either slow lookups or slow appends.

### 4.2 NVRAM log layout

The NVRAM log is the **primary** durable store. Segments are appended; nothing is ever rewritten in place (except during compaction, which is its own atomic operation).

```
{path}                          # Append-only segment data
  ├─ [offset 0]      Segment 1 bytes (len bytes)
  ├─ [offset N]      Segment 2 bytes
  └─ ...

{path}.segments                 # Segment metadata sidecar (JSON)
  {
    "SegmentId(1)": { offset, len, content_hash, ref_count, ... },
    "SegmentId(2)": { ... },
    ...
  }

{path}.tmp                      # Atomic-rename staging file
{path}.compacting               # Crash marker; presence at open() = recovery needed
```

**Crash safety**:
- `save_segment_map()` writes to `.tmp`, then atomic-renames over the live sidecar. A crash mid-write leaves the old sidecar intact.
- `open()` checks for `.tmp` and `.compacting` markers and recovers — orphaned `.tmp` files are dropped; `.compacting` triggers a re-validation pass.
- See [§10.6](#106-atomic-metadata-via-tmprename) for why this pattern.

**Compaction** (`NvramLog::compact()`) reclaims bytes from deleted/dedup-evicted segments by rewriting the live segments to a fresh file in-place, then truncating. The `.compacting` marker bounds the recovery window.

### 4.3 Capsule registry

The registry is a Sled-backed KV store holding:

- `Capsule` records keyed by `CapsuleId`
- A content-hash index: `ContentHash → SegmentId` for dedup lookup
- Audit events (`Event`) appended in order

The registry exposes a transactional interface — `begin_txn().await?.set_segment_metadata(...).await?.commit().await?` — so capsule + segment metadata mutate atomically. See [§9.3](#93-transactional-metadata).

### 4.4 Storage backends

`StorageBackend` is the trait that abstracts where segment bytes physically live. Current implementations:

| Backend | Crate | Use case |
|---|---|---|
| `InMemoryBackend` | `storage` | Tests; ephemeral workloads. `Arc<Mutex<Inner>>` — clones share state. |
| `NvramBackend` | `nvram-sim` | Primary durable store; append-only log on disk. |
| `TokioFsBackend` | `storage` | Generic file-per-segment backend; `.tmp`+rename atomicity. |
| `AutoFsBackend` | `storage` | Selects io_uring on Linux, tokio-fs elsewhere. |
| `CachedBackend<B>` | `storage` | Byte-bounded LRU wrapper around any backend; invalidates on commit. |

The capsule layer is backend-agnostic. The only assumption is that the backend implements the `StorageBackend` trait correctly (see [§9.2](#92-the-storagebackend-trait)).

---

## 5. The write flow

```
client bytes
    │
    ▼
┌─────────────────┐
│ 1. Segment      │  Split into 4 MiB chunks (LayoutPolicy::Fixed default)
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ 2. Compress     │  policy.compression → (compressed_bytes, algorithm_string)
└─────────────────┘    "identity" / "lz4:1" / "zstd:3"
    │
    ▼
┌─────────────────┐
│ 3. Hash         │  hash_content_with_algo(compressed_bytes, algorithm)
└─────────────────┘    → ContentHash (domain-separated by algorithm)
    │
    ▼
┌─────────────────┐
│ 4. Dedup check  │  registry.lookup_content(hash)
└─────────────────┘    HIT  → reuse SegmentId, inc ref_count, skip 5/6/7
    │ MISS              MISS → continue
    ▼
┌─────────────────┐
│ 5. Encrypt      │  tweak = first_16_bytes(content_hash)
└─────────────────┘    ciphertext = XTS-AES-256(key_pair, tweak, compressed_bytes)
    │
    ▼
┌─────────────────┐
│ 6. MAC          │  mac_key = BLAKE3(b"SPACE-BLAKE3-MAC-KEY-V1", key1, key2)
└─────────────────┘    integrity_tag = BLAKE3_keyed(mac_key, ciphertext || metadata)[..16]
    │
    ▼
┌─────────────────┐
│ 7. Append       │  backend.append(segment_id, final_bytes)
└─────────────────┘    + registry.set_segment_metadata(...)
    │
    ▼
SegmentId committed
```

### Stage-by-stage

**1. Segmentation.** The write pipeline splits the input into chunks. Default chunk size is 4 MiB (`SEGMENT_SIZE`). The `LayoutPolicy` can override this with adaptive entropy-driven sizing, ZNS zone graphs, or learned (Torch) strategies, but `Fixed(4 MiB)` is the only one always available.

**2. Compression.** Driven by `policy.compression`. Returns both the compressed bytes and the **algorithm string** (e.g., `"lz4:1"`, `"zstd:3"`, `"identity"`). The algorithm string is critical for step 3.

**3. Hash.** `hash_content_with_algo(compressed_bytes, algorithm_string)` produces a `ContentHash` that mixes the algorithm into the BLAKE3 input. This prevents cross-policy dedup collisions ([§10.4](#104-dedup-keys-need-algorithm-domain-separation)).

**4. Dedup check.** The registry looks up the content hash. On hit, the existing `SegmentId` is reused — no further work, just `ref_count += 1` on the existing segment and append the SegmentId to the new Capsule's segment list. On miss, proceed.

**5. Encrypt.** XTS-AES-256, tweak derived from the first 16 bytes of the content hash. Encryption is deterministic — same plaintext + same key + same content hash → same ciphertext. This is the property that lets encryption preserve dedup ([§10.2](#102-deterministic-encryption-preserves-dedup)).

**6. MAC.** BLAKE3-keyed MAC over `ciphertext || serialized_metadata`. The MAC key is derived from both XTS subkeys, ensuring the MAC binds to the encryption key in a way that key compromise of one doesn't trivially undermine the other. First 16 bytes of the keyed hash become the `integrity_tag`.

**7. Append.** Bytes go to the storage backend (NVRAM log); metadata goes to the registry under a transaction. On commit, the new SegmentId is durable.

### Important properties of the write path

- **Determinism**: write the same bytes under the same policy twice → same content hash → dedup hit on the second write. This is observable as `Capsule.deduped_bytes > 0` in the second Capsule.
- **No partial writes**: a Segment is fully appended-and-metadata'd or it doesn't exist. The transactional registry interface enforces this.
- **Order-independent dedup**: which Capsule "owns" a segment doesn't matter; ref counting makes the relationship symmetric.

The full implementation lives in `crates/capsule-registry/src/pipeline/legacy.rs` (and a modular variant gated by `SPACE_USE_MODULAR`).

---

## 6. The read flow

```
read(capsule_id, [range])
    │
    ▼
┌─────────────────┐
│ 1. Lookup       │  registry.get(capsule_id) → Capsule
└─────────────────┘
    │
    ▼  for each SegmentId in capsule.segments
┌─────────────────┐
│ 2. Plan range   │  Use segment.plain_len to skip segments outside range
└─────────────────┘    (no read, no decompress, no decrypt)
    │
    ▼
┌─────────────────┐
│ 3. Read bytes   │  backend.read(segment_id) → ciphertext (or plaintext if not encrypted)
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ 4. MAC verify   │  if segment.encrypted: verify_mac(...) — constant-time
└─────────────────┘    fail → ScrubResult::MacMismatch / read error
    │
    ▼
┌─────────────────┐
│ 5. Decrypt      │  if segment.encrypted: XTS-AES decrypt with stored key_version + tweak
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ 6. Decompress   │  if segment.compressed: dispatch on segment.compression_algo
└─────────────────┘
    │
    ▼
┌─────────────────┐
│ 7. Assemble     │  Concatenate decompressed segment bytes in segment order
└─────────────────┘    Slice to requested range if range read
    │
    ▼
client bytes
```

### Key points

- **Trust segment metadata, not policy.** Steps 4, 5, 6 branch on `segment.encrypted`, `segment.compressed`, `segment.compression_algo` — not on `capsule.policy`. A Capsule's policy describes how it was *written*; segment metadata describes how it actually *is*. A Capsule transformed through PODMS swarm migration may have heterogeneous segments; relying on `capsule.policy` would corrupt the read.

- **Range reads skip work.** With `segment.plain_len` set, the planner can skip both reading and decompressing segments that lie entirely outside the requested range. This is the difference between a 2 GB read for a 4 KiB request and a 4 KiB read.

- **MAC before decrypt.** The MAC tag is verified against the ciphertext *before* decryption. If the ciphertext was tampered with, decryption never runs — we don't process untrusted input through the AES core.

- **Constant-time MAC compare.** Uses `subtle::ConstantTimeEq` to prevent timing side-channels on tag verification.

---

## 7. Deletion, ref counting, and GC

Capsules are deleted by reference; segments are deleted by reaching ref_count zero.

### Ref count lifecycle

```
Segment lifecycle:
  Write new        : ref_count = 1
  Dedup hit        : ref_count += 1 (atomic via NvramLog)
  Capsule deleted  : ref_count -= 1 for each of its segments
  ref_count == 0   : eligible for GC
```

### Delete path

`DELETE capsule_id`:

1. Look up the Capsule.
2. For each `SegmentId` in `capsule.segments`: `nvram.decrement_refcount(seg_id)`.
3. Remove the Capsule record from the registry.
4. Emit `Event::CapsuleDeleted`.

Segments with `ref_count > 0` after the decrement remain — they belong to other Capsules.

### Garbage collection

`GarbageCollector::sweep()` (in `capsule-registry/src/gc.rs`):

```
for each segment in backend.segment_ids():
    if segment.ref_count == 0:
        registry.deregister_content(segment.content_hash, segment.id)
        backend.remove_segment(segment.id)
```

GC removes the metadata entry and the content-hash index entry. **Bytes remain in the log** until compaction (`NvramLog::compact()`) rewrites the live segments to a fresh file and truncates. The two-step design — GC marks; compaction reclaims — lets ref-count decrement be cheap (touch one record) while keeping bulk reclamation infrequent.

### Race-freedom

The dedup-hit-vs-GC race is the classic worry: a segment hits zero just as a new write is about to dedup-hit it. SPACE serializes these through the NvramLog mutex on `ref_count`: a dedup hit either succeeds (incrementing from 1 → 2) or finds the segment already gone (forcing a fresh write). There is no window where a segment is both reachable and eligible for collection.

---

## 8. Scrub: continuous integrity

Capsules are durable only if their bytes still match their hashes and MACs. Scrub is the background loop that proves this.

### Two flavors

| Kind | What it verifies | I/O | CPU |
|---|---|---|---|
| **Light** | Stored byte count matches `segment.len` | Full read | None |
| **Deep** | MAC tag (encrypted) or content hash (unencrypted) | Full read | BLAKE3 / AES |

Both run on configurable intervals (defaults: light = 24h, deep = 7d). Each segment tracks its last-checked timestamps; a cycle scrubs only segments that are due.

### Deep scrub of encrypted segments

1. Read ciphertext from backend.
2. Look up `key_version` → retrieve `XtsKeyPair`.
3. `verify_mac(ciphertext, metadata, key_pair)` — constant-time.
4. Failure → `ScrubResult::MacMismatch` (durable corruption or tampering).

### Deep scrub of unencrypted segments

1. Read bytes from backend.
2. `verify_content_hash(expected_hash, data, algorithm_string)` → `VerifyOutcome::{Matched, LegacyMatched, Mismatched}`.
3. `Matched` → healthy. `Mismatched` → `ContentCorrupted`. `LegacyMatched` → healthy *but* the segment was written before the algorithm-domain-separated hash fix and is verifiable only via the bare BLAKE3 fallback.

The `legacy_hash_hits` counter in `ScrubReport` is the operator's gauge for retiring the compatibility window. When it trends to zero across the cluster, the legacy fallback can be removed.

### Rate limiting

`ScrubConfig.max_bytes_per_sec` enables a segment-size-aware token bucket so scrub doesn't crowd out user I/O. The `inter_segment_delay` is the floor — scrub never runs hotter than the configured pace.

### State machine

```
Idle → Running(kind) → Completed { kind, errors } → Idle
```

Published via a `tokio::sync::watch` channel. Consumers observe transitions without polling. The deep-scrub MAC/BLAKE3 work runs inside `spawn_blocking` so the async executor isn't stalled by CPU-heavy verification.

See `crates/common/src/scrub.rs` and `crates/capsule-registry/src/scrub_executor.rs`.

---

## 9. Architecture: where each responsibility lives

### 9.1 Crate map

```
common         ─ Capsule, Segment, ContentHash, Policy, all traits
                 (no I/O, no async runtime, no crypto impl — just types & contracts)

capsule-registry ─ The registry. WritePipeline. ScrubExecutor. GC.
                   Transactional Sled metadata. Audit events.

storage        ─ StorageBackend implementations: InMemory, TokioFs, AutoFs, Cached.
                 io_uring optimization on Linux.

nvram-sim      ─ The primary durable backend. Append-only log + JSON sidecar.
                 Atomic rename, compaction, ref count tracking.

dedup          ─ hash_content, hash_content_with_algo, verify_content_hash.
                 Blake3Deduper (in-memory hash → SegmentId index).

encryption     ─ XTS-AES-256 cipher. BLAKE3-MAC. KeyManager (HKDF derivation).
                 Tweak derivation from content hash.

compression    ─ LZ4 and Zstd. Adaptive entropy-based selection.

tiering        ─ Hot/cold heatmap. Driven by Segment.access_count.

(protocol-*)   ─ S3, NFS, block, FUSE, CSI, NVMe-oF adapters.
                 None of these own bytes — they project capsules.
```

### 9.2 The `StorageBackend` trait

```rust
pub trait StorageBackend: Send + Sync {
    type Transaction: StorageTransaction;

    // Mutating (&mut self):
    fn append(&mut self, segment: SegmentId, data: &[u8]) -> BoxFuture<'_, Result<()>>;
    fn delete(&mut self, segment: SegmentId) -> BoxFuture<'_, Result<()>>;
    fn begin_txn(&mut self) -> BoxFuture<'_, Result<Self::Transaction>>;

    // Non-mutating (&self):
    fn read(&self, segment: SegmentId) -> BoxFuture<'_, Result<Vec<u8>>>;
    fn metadata(&self, segment: SegmentId) -> BoxFuture<'_, Result<Segment>>;
    fn segment_ids(&self) -> BoxFuture<'_, Result<Vec<SegmentId>>>;
    fn used_bytes(&self) -> BoxFuture<'_, Result<u64>> { /* default Ok(0) */ }
}
```

Two contracts worth emphasizing:

- **`&self` reads.** Multiple concurrent reads are expected and the trait does not gate them. Backends that need interior mutability use `Arc<Mutex<Inner>>` or similar.
- **Metadata only via transactions.** Direct field-level mutation is intentionally not in the trait surface. `begin_txn().await?.set_segment_metadata(id, seg).await?.commit().await?` is the single path. This is what makes write/dedup/GC race-free.

### 9.3 Transactional metadata

The registry's transaction abstraction wraps Sled's batch primitives. Key properties:

- **Atomicity**: capsule + all its segment metadata updates commit together or not at all.
- **Isolation**: in-flight transactions see a snapshot; commit is serialized.
- **Crash safety**: on crash mid-commit, the batch is either applied or dropped — never partial.

This is why metadata invariants ("a referenced segment exists", "a content hash maps to a real segment") hold across crashes.

### 9.4 PODMS swarm behavior (feature-gated)

When `podms` is enabled, Capsules gain `apply_transform` and `on_migrate` hooks that let them re-shape themselves during migration: decrypt under source policy, transcode compression if it differs, re-encrypt under destination policy. This is how a Capsule moves between zones with different keys or compression settings without lossy intermediate states.

See the `SwarmBehavior` implementation in `crates/common/src/lib.rs` and [docs/podms.md](podms.md).

---

## 10. Design rationale

Each subsection below covers one load-bearing decision and the alternative we rejected.

### 10.1 Why 128-bit UUIDs for capsule identity

**Decision**: `CapsuleId` is a 128-bit UUID v4, generated locally.

**Alternative**: monotonic 64-bit ID from a coordinator, or content-hash-as-ID.

**Why**: SPACE has to work across zones, federated clusters, and disconnected edge sites. A coordinator on the write path is unacceptable for an air-gapped edge node. Content-hash-as-ID was tempting but breaks the moment a Capsule is re-encrypted or transcoded — its bytes change, but the *logical* identity must not (see [§2.1](#21-identity-vs-representation)). UUIDs decouple identity from representation, allowing storage-layer transformations to rewrite segments while preserving the references that views, audit logs, and federation peers hold.

128 bits is the standard collision-safe width for uncoordinated generation. 64 bits has a real collision risk at cluster scale (birthday bound ≈ 2³² capsules).

### 10.2 Deterministic encryption preserves dedup

**Decision**: XTS-AES-256 with the tweak derived from the content hash (first 16 bytes).

**Alternative**: AES-GCM with a random nonce per segment.

**Why**: GCM with random nonces breaks dedup — encrypting the same plaintext twice produces different ciphertext, so two capsules with identical content store two copies. SPACE chose XTS with a deterministic tweak so that identical plaintext + identical key produces identical ciphertext, preserving dedup *after* encryption.

The price: XTS is technically less robust against chosen-ciphertext attacks than GCM. We pay this back with the BLAKE3-MAC layer ([§10.3](#103-mac-on-top-of-xts)), which gives us authenticated encryption properties at the cost of one extra hash per segment. The combined construction is XTS-then-MAC, not GCM.

The tweak derives from the content hash, so the tweak is also dedup-determined. This is unusual — XTS tweaks are conventionally sector numbers. Using the hash means we don't need a stable address space to encrypt, which is necessary because segment IDs can shift (compaction).

### 10.3 MAC on top of XTS

**Decision**: After XTS encryption, compute a BLAKE3-keyed MAC over `ciphertext || serialized_metadata`. Store the first 16 bytes as `integrity_tag`.

**Alternative**: rely on XTS alone, or use AES-GCM (which bundles auth).

**Why**: XTS is a length-preserving cipher with no authentication — corrupting ciphertext produces corrupt plaintext silently. The MAC layer turns this into authenticated encryption. The MAC also covers the encryption metadata (version, key version, tweak, length) so an attacker cannot swap segments between Capsules and have them decrypt cleanly.

BLAKE3 is chosen for speed (>2 GB/s on commodity x86). The MAC key is derived deterministically from both XTS subkeys: `BLAKE3(b"SPACE-BLAKE3-MAC-KEY-V1", key1, key2)`. This binds the MAC to the encryption key without requiring a separate key store entry.

Verification uses constant-time comparison.

### 10.4 Dedup keys need algorithm domain separation

**Decision**: `hash_content_with_algo(data, algo)` mixes the compression algorithm into the BLAKE3 input. Pipeline writes use this function; bare `hash_content(data)` is reserved for the legacy verification fallback.

**Alternative**: hash only the stored bytes.

**Why**: without algorithm separation, this construction silently corrupts data:

- Capsule A: write raw LZ4-framed bytes under `CompressionPolicy::None`. Stored as-is with `compressed=false`.
- Capsule B: write the equivalent plaintext under `CompressionPolicy::LZ4`. Compresses to the *same* LZ4 frame, stored with `compressed=true`.

Both produce the same `content_hash` if hashed naively. B's write dedup-hits A's segment. On read, B's reader sees `segment.compressed=true` (truth!), invokes the LZ4 decompressor on what *is* a valid LZ4 frame, and gets... the original plaintext (correct in this constructed case, but the invariant only holds by accident; any other format pair silently corrupts).

The fix domain-separates the hash by algorithm. The two writes now produce distinct hashes, distinct segments, no false dedup. The [proptest harness](../crates/capsule-registry/tests/proptest_pipeline.rs) `prop_no_cross_policy_dedup_collision` enforces this invariant.

### 10.5 Content hash over compressed pre-encryption bytes

**Decision**: `content_hash = BLAKE3(compressed_bytes, algo)`. Not over plaintext. Not over ciphertext.

**Alternative**: hash plaintext (dedup by logical content) or hash ciphertext (dedup by stored representation).

**Why**:
- **Hash plaintext**: dedup before compression. Tempting, but means two capsules with the same plaintext but different compression policies would share a segment — the stored bytes differ, so this fails immediately.
- **Hash ciphertext**: dedup after encryption. Requires the tweak to be content-independent, which means dedup is broken or tweaks leak content. Either way, no good.
- **Hash compressed-pre-encryption**: dedup matches stored compressed bytes; tweak can derive from the hash deterministically; encryption preserves dedup. This is the only point in the pipeline where all three properties align.

### 10.6 Atomic metadata via `.tmp` + rename

**Decision**: `NvramLog::save_segment_map` writes the new sidecar to `{path}.segments.tmp`, then atomic-renames over the live file.

**Alternative**: overwrite in place.

**Why**: a crash mid-overwrite leaves the sidecar truncated and unparseable, taking the entire segment map down. With `.tmp` + rename, a crash before rename leaves the old sidecar intact; a crash after rename leaves the new sidecar intact. The window between is the kernel's rename atomicity, which on POSIX is a single inode flip. On Windows, the equivalent is `MoveFileEx(MOVEFILE_REPLACE_EXISTING)`.

The `.compacting` marker handles a different failure mode: a crash mid-compaction (rewriting the segment log itself, not the sidecar) might leave dangling references. The marker's presence at `open()` signals "re-validate" before accepting reads.

### 10.7 4 MiB default segment size

**Decision**: `SEGMENT_SIZE = 4 * 1024 * 1024`.

**Alternative**: smaller (better dedup granularity) or larger (less per-segment overhead).

**Why**: 4 MiB is the empirical sweet spot across the workloads SPACE targets:
- Large enough that per-segment metadata (offset, hash, MAC tag, ~120 bytes total) is well under 0.01% of the segment.
- Small enough that range reads waste at most 4 MiB.
- Small enough that dedup hits are common for many real-world files (video frames, container layers, source archives).
- Aligned with common SSD erase block and io_uring submission sizes.

The `LayoutPolicy` can override this — adaptive entropy compression, ZNS zone graphs, and learned strategies all produce variable-sized segments. The 4 MiB default is the fallback when no smarter strategy is configured.

### 10.8 Append-only log as primary store

**Decision**: NVRAM is append-only; modifications happen via new segments and ref count changes; reclamation happens via compaction.

**Alternative**: in-place update with a free list.

**Why**: append-only matches NVRAM/flash physics (no rewrites = no write amplification = longer device life), matches replication semantics (ship the log forward), and dramatically simplifies crash recovery (no partial-write torn states). The compaction cost is amortized and runs in the background; it doesn't block writes.

The trade-off is space overhead: GC'd segments occupy bytes until the next compaction. For workloads with rapid churn this is real, but compaction frequency is policy-tunable (`compact_interval_secs`).

### 10.9 Two-store split: NVRAM log + Sled registry

**Decision**: bytes in NVRAM log, metadata in Sled.

**Alternative**: a single store for both.

**Why**: the access patterns are fundamentally different. Segment bytes want streaming sequential I/O, large blocks, and append semantics. Capsule metadata wants point lookups, transactional updates, and small records. Fusing them means either the metadata path is slow (full-log scans) or the byte path is slow (random-write database pages).

Sled gives transactional semantics for metadata. NVRAM log gives append throughput for bytes. The transactional commit handoff between the two is the single point where atomicity matters — and is the only place we use a coordinated write barrier.

### 10.10 Trust segment metadata over policy on read

**Decision**: read-path branches on `segment.compressed`, `segment.encrypted`, `segment.compression_algo`. Never on `capsule.policy`.

**Alternative**: read `capsule.policy` and infer how to decode.

**Why**: a Capsule's `policy` field reflects the *current* intended representation, but transformation is not atomic across all segments — during a migration, key rotation, or transcoding sweep, segments may be heterogeneous (some already rewritten, some not). Even at rest, key rotation can leave segments at different `key_version`s. Branching on the Capsule-level policy would silently corrupt reads any time stored segments diverge from it. Branching on segment metadata is always correct because that's what's actually on disk.

---

## 11. Invariants

These are the invariants the system maintains. They are tested by the proptest harness in [crates/capsule-registry/tests/proptest_pipeline.rs](../crates/capsule-registry/tests/proptest_pipeline.rs).

### Round-trip

For any byte sequence `B` and any policy `P`:
```
read(write(B, P)) == B
```
Tested by `prop_roundtrip_unencrypted`, `prop_roundtrip_encrypted`, and boundary cases (empty, single byte, 4 MiB ± epsilon).

### Dedup determinism

For any byte sequence `B` written N times under the same policy:
```
∀i,j ∈ [0,N): capsule[i].segments == capsule[j].segments
∀seg ∈ segments: seg.ref_count == N
```
Tested by `prop_dedup_same_payload_shares_segments`.

### Content separation

For distinct byte sequences `B₁ ≠ B₂` (modulo hash collision):
```
capsule_1.segments ∩ capsule_2.segments == ∅
```
Tested by `prop_distinct_payloads_distinct_segments`.

### Cross-policy isolation

For the constructed collision pair (raw LZ4 frame vs plaintext compressed under LZ4):
```
capsule_raw.segments ∩ capsule_compressed.segments == ∅
∧ read(capsule_raw) == raw_input
∧ read(capsule_compressed) == plaintext_input
```
Tested by `prop_no_cross_policy_dedup_collision`. This is the regression test for the dedup domain-separation fix.

### Ref count consistency

```
seg.ref_count == count(capsules referencing seg.id)
seg.ref_count == 0 ⟹ seg eligible for GC
```

### Crash recovery

After arbitrary crash during write or compaction:
```
open() succeeds
all committed Capsules remain readable
no Capsule is partially visible
```

### Segment metadata is truth

```
read_path branches on segment.{compressed, encrypted, compression_algo, key_version}
never on capsule.policy
```

---

## 12. Failure modes and recovery

| Failure | Detection | Recovery |
|---|---|---|
| **Bit rot in segment bytes** | Deep scrub: content hash or MAC mismatch | `ScrubResult::ContentCorrupted` / `MacMismatch`. Operator restores from replica or accepts loss. |
| **Truncated segment file** | Light scrub: actual_len ≠ metadata.len | `ScrubResult::MetadataMismatch`. Same as above. |
| **Lost sidecar** | `open()` finds no `.segments` file | Reconstruction from log scan (boot-time recovery). |
| **Crash mid-`save_segment_map`** | `.tmp` exists at `open()` | Drop `.tmp`; old sidecar still valid. |
| **Crash mid-compaction** | `.compacting` marker present | Re-validate live segments against rewritten log; resume or abort compaction. |
| **Crash mid-append** | Log length > sidecar's max offset | Truncate log to last recorded offset. The partial segment was never committed; safe to discard. |
| **Sled corruption** | Sled checksum failure on read | Restore registry from latest snapshot + replay audit log. |
| **Wrong key on read** | MAC verify fails | Read fails with auth error. No plaintext is exposed. |
| **Key rotated, old key gone** | `key_manager.get_key(version)` returns None for stored `key_version` | Read fails. Key retention is the operator's responsibility. |
| **Dedup-hit-then-GC race** | Cannot occur — ref_count operations are serialized through NvramLog mutex | n/a |
| **Cross-policy hash collision** | `prop_no_cross_policy_dedup_collision` enforces non-occurrence | n/a — prevented by `hash_content_with_algo` domain separation. |
| **Pre-fix bare-BLAKE3 segment encountered by scrub** | `verify_content_hash` returns `LegacyMatched` | Healthy; counted in `legacy_hash_hits`. Operator tracks gauge to retire compatibility window. |

---

## 13. Glossary

| Term | Meaning |
|---|---|
| **Capsule** | The atomic, content-addressed, policy-bound unit of storage. 128-bit identity. |
| **Segment** | A chunk of a Capsule (default 4 MiB). The unit of dedup, compression, and encryption. |
| **CapsuleId** | UUID v4 wrapped in a newtype. |
| **SegmentId** | Monotonic u64 within an NVRAM log. |
| **ContentHash** | Hex-encoded BLAKE3 over compressed pre-encryption bytes, with algorithm domain separation. |
| **Policy** | The compression/encryption/layout/federation/transform bundle currently active on a Capsule. Updated in place by storage-layer transformations (PODMS migration, key rotation, transcoding); see [§2.1](#21-identity-vs-representation). |
| **Integrity tag** | First 16 bytes of `BLAKE3_keyed(mac_key, ciphertext ‖ metadata)`. The MAC. |
| **Tweak** | 16-byte XTS tweak derived from the content hash. |
| **Light scrub** | Background verification of stored byte counts. |
| **Deep scrub** | Background verification of MAC tags (encrypted) or content hashes (unencrypted). |
| **Legacy hash hit** | A segment that verifies only under the pre-fix bare BLAKE3 hash; tracked in `ScrubReport.legacy_hash_hits`. |
| **Backend** | An implementation of `StorageBackend` — where segment bytes physically live. |
| **Registry** | The Sled-backed metadata store: Capsules, content-hash index, audit log. |
| **NVRAM log** | The primary durable backend. Append-only log + JSON sidecar. |
| **Compaction** | Rewriting the live segments of an NVRAM log to reclaim space from GC'd segments. |
| **PODMS** | Policy-Orchestrated Disaggregated Mesh Scaling. Feature-gated distributed mode. |
| **Sovereignty** | A PODMS policy attribute controlling whether a Capsule may be replicated/migrated across zones. |

---

## 14. Cross-references

### Source code

- Core types: [crates/common/src/lib.rs](../crates/common/src/lib.rs)
- Policy: [crates/common/src/policy.rs](../crates/common/src/policy.rs)
- Traits: [crates/common/src/traits.rs](../crates/common/src/traits.rs)
- Scrub types: [crates/common/src/scrub.rs](../crates/common/src/scrub.rs)
- Write/read pipeline: [crates/capsule-registry/src/pipeline/](../crates/capsule-registry/src/pipeline/)
- Scrub executor: [crates/capsule-registry/src/scrub_executor.rs](../crates/capsule-registry/src/scrub_executor.rs)
- GC: [crates/capsule-registry/src/gc.rs](../crates/capsule-registry/src/gc.rs)
- NVRAM log: [crates/nvram-sim/src/lib.rs](../crates/nvram-sim/src/lib.rs)
- Dedup: [crates/dedup/src/lib.rs](../crates/dedup/src/lib.rs)
- Encryption: [crates/encryption/src/](../crates/encryption/src/)
- Proptest invariants: [crates/capsule-registry/tests/proptest_pipeline.rs](../crates/capsule-registry/tests/proptest_pipeline.rs)

### Related documentation

- [Architecture Overview](architecture.md) — platform-level diagrams, system context.
- [CapsuleFlow Layout Engine](capsuleflow.md) — Phase 3 layout strategies.
- [Protocol Views](protocol_views.md) — S3, NFS, block, FUSE, CSI, NVMe-oF facades.
- [Encryption Implementation](implementation/ENCRYPTION_IMPLEMENTATION.md) — XTS-AES-256 + BLAKE3-MAC deep dive.
- [Dedup Implementation](implementation/DEDUP_IMPLEMENTATION.md) — Blake3Deduper details.
- [Dependency Security](dependency-security.md) — crypto dependency audit trail.
- [Patentable Concepts](patentable_concepts.md) — novel-claims summary.
- [PODMS Scaling](podms.md) — distributed mesh mode.
- [Future State Architecture](future_state_architecture.md) — direction beyond v0.2.

---

*Last updated: 2026-05-16. If you change any invariant in §11, update both the corresponding proptest and this document.*
