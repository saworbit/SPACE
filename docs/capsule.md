# The Capsule

> The storage object on this branch: one id, an ordered list of segments, and a policy for compression, dedup, and encryption.
>
> **Reading this on the post-reset `main` branch:** §1–§2 describe the design goal, which includes protocol views, migration, key rotation and replication. On this branch only the S3 view and `spacectl` exist, and capsules are never re-encrypted, transcoded, migrated or replicated. §3–§12 describe the code as it is, with links to open issues where it falls short. The mesh, federation and tiering design is on `archive/pre-reset-2026-10`.

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

SPACE makes a different bet: **separate the durable representation of data from the protocol you use to talk to it**. The durable representation is the Capsule. Protocols (S3 on this branch; NFS, block, NVMe-oF, FUSE and CSI in the archived design) are *views* projected on top of capsules — none of them own bytes.

This means:

- The same bytes are simultaneously a file, a block, and an object — no copies.
- Dedup, compression, and encryption are properties of the capsule, not the protocol.
- Encryption keys, integrity tags, and content hashes survive protocol changes — you don't re-encrypt to switch from S3 to NFS.
- Adding a protocol is writing an adapter, not building a storage stack.

### The cost

The bet is non-trivial:

- **Every byte must fit a single, opinionated representation.** Compression, encryption, dedup, and integrity verification all assume capsule structure. You cannot opt out of the abstraction for "fast path" data.
- **Encryption must be deterministic to preserve dedup.** SPACE uses XTS-AES-256 with tweaks derived from a hash of each segment's bytes. This is unusual — most systems pick deterministic-or-secure, not both. (See [§10.2](#102-deterministic-encryption-preserves-dedup).)
- **Metadata is on the hot path.** Every read touches Sled (for capsule lookup), the NVRAM segment map (for offsets), and the key manager (for decryption). Metadata cannot be slow.

The rest of this document explains how SPACE pays that cost.

---

## 2. What a Capsule is

A **Capsule** is a content-addressed, policy-bound collection of segments with a stable 128-bit identity.

- **Stable identity**: `CapsuleId(Uuid)` is permanent for the Capsule's lifetime. It survives re-encryption, key rotation, compression transcoding, migration between zones, replication, and protocol projection (none of those transformations exist on this branch yet). Generated without coordination.
- **Immutable logical content**: what `read(capsule_id)` returns is preserved across every storage-layer transformation. The bytes a client sees do not change because the Capsule moved zones or rotated keys.
- **Mutable representation**: the segment list, per-segment content hashes, encryption metadata, and the `Policy` field itself **can change in place** when the Capsule is transformed by PODMS swarm migration, key rotation, or compression transcoding. See [§2.1](#21-identity-vs-representation) below.
- **Content-addressed**: every segment carries a `content_hash` (BLAKE3 over compressed-pre-encryption bytes) that uniquely identifies its payload within a store.
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

- **Protocol-layer mutations** (a client overwriting a file at `/foo.txt`, rewriting block offset 4096 of `vol0`, PUT-ing a new object body to `s3://bucket/key`) produce a **new** Capsule with a new `CapsuleId`. The view's metadata is updated to point at the new Capsule; the old one is dereferenced and may be GC'd. (The S3 view doesn't dereference the old capsule yet; it stays in the store, [#86](https://github.com/saworbit/SPACE/issues/86).) The Capsule abstraction is *not* in-place mutable from the protocol surface.
- **Storage-layer transformations** (PODMS swarm migration, key rotation, compression transcoding, tier promotion/demotion) **preserve** `CapsuleId` and logical content while rewriting the on-disk representation. Audit log continuity is preserved; references at the view layer remain valid.

This is what makes federated migration tractable: you can move a Capsule from `us-west` to `eu-central` (with re-encryption under the destination's key) without touching any file path, S3 key, or block volume that points at it.

The data-motion implementation (`crates/scaling`) was removed in the reset and is on `archive/pre-reset-2026-10`.

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
| `created_at` | Unix seconds at creation | Recorded only; nothing on this branch reads it yet. |
| `policy` | The compression/encryption/layout policy the Capsule was written with. Set at write time; the storage-layer transformations that would update it aren't implemented. | Read-path code must NOT branch on this — branch on segment metadata. See [§2.1](#21-identity-vs-representation) and [§3.6](#36-policy). |
| `deduped_bytes` | Compressed bytes that hit existing segments during write | Operator visibility into dedup effectiveness per Capsule. |

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
| `plain_len` | Write pipeline (segment size before compression; `None` for `spacectl put`) | Range-read planner (skip segments outside range without decompressing) |
| `compressed`, `compression_algo` | Write pipeline | Reader (choose decompressor); scrub (verify content_hash with same algo) |
| `content_hash` | Dedup stage, pre-encryption | Dedup lookup; scrub (unencrypted deep verification) |
| `ref_count` | Inc on dedup hit / new capsule; dec on delete | Delete path (removes the segment at the last reference); GC |
| `deduplicated` | Write pipeline (true if any capsule shares this segment) | Operator visibility |
| `access_count` | Not updated on read (stays 0) | Nothing (the tiering heatmap that used it was removed) |
| `encryption_version`, `key_version`, `tweak_nonce`, `integrity_tag`, `encrypted` | Encryption stage | Reader (key selection, MAC verify); scrub (deep MAC check) |
| `pq_ciphertext`, `pq_nonce` | Nothing: always `None` (there's no post-quantum implementation) | Nothing |

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
}
```

Presets (`Policy::default()`, `text_optimized()`, `precompressed()`, `edge_optimized()`, `encrypted()`, `encrypted_compressed()`) provide named bundles for common workloads. Custom policies are constructed field-by-field.

On this branch only `compression`, `dedupe`, `encryption` and `layout` change what the write path does:

- With `dedupe = false`, every segment is new, and its hash is not registered, so later writes can't attach to it.
- `compact_interval_secs`, `erasure_profile`, `federation` and `transform` are stored, but nothing reads them.
- `CryptoProfile::HybridKyber` only affects `Policy::validate`; there's no ML-KEM code.
- `spacectl create` and S3 PUT always use `Policy::default()`, which has encryption disabled ([#82](https://github.com/saworbit/SPACE/issues/82)).

### 3.7 `Event`

The `Event` enum defines audit events: `CapsuleCreated`, `CapsuleRead`, `CapsuleDeleted`, `SegmentAppended`, `DedupHit`, `AuditHeartbeat`. Nothing on this branch emits them, and there's no audit log.

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
{path}                          # Append-only segment data, no per-record header
  ├─ [offset 0]      Segment 0 bytes (len bytes)
  ├─ [offset N]      Segment 1 bytes
  └─ ...

{path}.segments                 # Segment metadata sidecar (pretty-printed JSON)
  {
    "0": { "id": 0, "offset": 0, "len": 42, "content_hash": "…", "ref_count": 1, … },
    "1": { … },
    …
  }

{path}.segments.tmp             # Staging file for the atomic rename
{path}.segments.compacting      # Marker present while compact() runs
```

**Crash safety**:
- `save_segment_map()` writes the whole sidecar to `.segments.tmp`, then renames it over `.segments`. A crash before the rename leaves the old sidecar intact. Neither the file nor its directory is fsynced, so after a power loss the sidecar can lag behind the data, which is fsynced ([#96](https://github.com/saworbit/SPACE/issues/96)).
- `open()` promotes `.segments.tmp` only when `.segments` is missing; otherwise a leftover `.tmp` is ignored. If `.segments.compacting` exists, `open()` refuses to open the log.
- The log has no record framing, so the sidecar is the only map of the data. Losing it makes every segment unreadable ([#96](https://github.com/saworbit/SPACE/issues/96)).
- See [§10.6](#106-atomic-metadata-via-tmp--rename) for why this pattern.

**Compaction** (`NvramLog::compact()`) reclaims bytes from deleted segments. It copies the live segments to the start of the same file and truncates the rest, holding all live data in memory while it runs. Nothing schedules it yet ([#97](https://github.com/saworbit/SPACE/issues/97), [#108](https://github.com/saworbit/SPACE/issues/108)).

### 4.3 Capsule registry

The registry is a Sled-backed KV store holding:

- `Capsule` records keyed by `CapsuleId`
- A content-hash index: `ContentHash → SegmentId` for dedup lookup
- A counter for allocating `SegmentId`s

Each update is a single sled insert followed by a flush. Segment metadata isn't in the registry: it lives in the NVRAM sidecar and is changed through `StorageBackend::begin_txn()…commit()`. The two stores are committed separately; see [§9.3](#93-metadata-commits).

### 4.4 Storage backends

`StorageBackend` is the trait that abstracts where segment bytes physically live. Current implementations:

| Backend | Crate | Use case |
|---|---|---|
| `InMemoryBackend` | `storage` | Tests; ephemeral workloads. `Arc<Mutex<Inner>>` — clones share state. |
| `NvramBackend` | `storage` (wraps `nvram_sim::NvramLog`) | Primary durable store; append-only log on disk. |
| `TokioFsBackend` | `storage` | Generic file-per-segment backend; `.tmp`+rename atomicity. |
| `AutoFsBackend` | `storage` | Uses io_uring on Linux when `storage` is built with the `uring` feature, tokio-fs otherwise. `serve-s3` uses it when `SPACE_STORAGE_ROOT` is set. |
| `CachedBackend<B>` | `storage` | Byte-bounded LRU wrapper around any backend; invalidates on commit. Not used by `spacectl` or the S3 view. |

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
│ 5. Encrypt      │  tweak = first_16_bytes(BLAKE3(compressed_bytes))
└─────────────────┘    ciphertext = XTS-AES-256(key_pair, tweak, compressed_bytes)
    │
    ▼
┌─────────────────┐
│ 6. MAC          │  mac_key = BLAKE3(b"SPACE-BLAKE3-MAC-KEY-V1", key1, key2)
└─────────────────┘    integrity_tag = BLAKE3_keyed(mac_key, ciphertext || metadata)[..16]
    │
    ▼
┌─────────────────┐
│ 7. Append       │  txn.append(segment_id, final_bytes)
└─────────────────┘    + txn.set_segment_metadata(...); registry.register_content(hash)
    │
    ▼  (after every segment)
┌─────────────────┐
│ 8. Capsule      │  registry: insert Capsule { id, size, segments, policy, ... }
└─────────────────┘
```

### Stage-by-stage

**1. Segmentation.** The write pipeline splits the input into chunks. Default chunk size is 4 MiB (`SEGMENT_SIZE`). `LayoutPolicy` can set another size, either `Fixed { segment_size }` or a zone size with `ZnsGraph { zone_size_mib }`. `AdaptiveEntropy`, `Learned` and `QuantumReady` all fall back to 4 MiB, so there's no variable-size chunking. `Policy::validate` rejects a size of 0 or above 16 MiB (`MAX_SEGMENT_SIZE`), which keeps each segment within the XTS data unit limit.

**2. Compression.** Driven by `policy.compression`. Returns both the compressed bytes and the **algorithm string** (e.g., `"lz4:1"`, `"zstd:3"`, `"identity"`). The algorithm string is critical for step 3.

**3. Hash.** `hash_content_with_algo(compressed_bytes, algorithm_string)` produces a `ContentHash` that mixes the algorithm into the BLAKE3 input. This prevents cross-policy dedup collisions ([§10.4](#104-dedup-keys-need-algorithm-domain-separation)).

**4. Dedup check.** The registry looks up the content hash. On hit, the existing `SegmentId` is reused — no further work, just `ref_count += 1` on the existing segment and append the SegmentId to the new Capsule's segment list. On miss, proceed. The hash domain includes the encryption mode, so encrypted and plaintext writes never share a segment. With `policy.dedupe = false` the lookup is skipped.

**5. Encrypt.** Runs only when the policy enables encryption. XTS-AES-256, with the tweak taken from the first 16 bytes of a plain BLAKE3 hash of the compressed segment (not the algorithm-separated `content_hash`). Encryption is deterministic: the same compressed bytes under the same key version give the same ciphertext. This is the property that lets encryption preserve dedup ([§10.2](#102-deterministic-encryption-preserves-dedup)). XTS needs at least 16 bytes, so a shorter segment is stored uncompressed and zero-padded to 16 bytes. The read strips the padding using `plain_len`.

**6. MAC.** BLAKE3-keyed MAC over the ciphertext plus the encryption metadata (encryption version, key version, tweak, ciphertext length). The MAC key is derived from both XTS subkeys. The first 16 bytes of the keyed hash become the `integrity_tag`.

**7. Append.** Bytes are appended to the NVRAM log and fsynced. The segment record is written to the sidecar through a storage transaction, and then the content hash is registered in sled.

**8. Capsule.** After the last segment, the capsule record is inserted into sled.

### Important properties of the write path

- **Determinism**: write the same bytes under the same policy twice → same content hash → dedup hit on the second write. This is observable as `Capsule.deduped_bytes > 0` in the second Capsule.
- **No partially visible capsules**: the capsule record is inserted only after every segment is stored, so a failed or interrupted write never shows up in `list` or `read`. Any segments it had already stored stay behind, unreferenced, until GC runs ([#97](https://github.com/saworbit/SPACE/issues/97)).
- **Order-independent dedup**: which Capsule "owns" a segment doesn't matter; ref counting makes the relationship symmetric.

The implementation is `Pipeline::write_capsule` in [crates/pipeline/src/lib.rs](../crates/pipeline/src/lib.rs). `capsule_registry::pipeline::WritePipeline` and `capsule_registry::modular_pipeline::RegistryPipelineHandle` wrap it.

---

## 6. The read flow

```
read(capsule_id, [range])
    │
    ▼
┌─────────────────┐
│ 1. Lookup       │  registry.lookup(capsule_id) → Capsule
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
└─────────────────┘    fail → read error
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

- **Trust segment metadata, not policy.** Steps 4, 5, 6 branch on `segment.encrypted`, `segment.compressed`, `segment.compression_algo` — not on `capsule.policy`. A Capsule's policy describes how it was *written*; segment metadata describes how it actually *is*. Through dedup, a capsule can reference segments written under a different compression policy, so relying on `capsule.policy` would corrupt the read.

- **Range reads skip work.** With `segment.plain_len` set, `read_range` skips segments that end before the requested range. A segment that overlaps the range is still read, decrypted and decompressed in full, so a 4 KiB range costs one or two segment reads (up to 8 MiB), not 4 KiB. The S3 view doesn't use range reads yet ([#99](https://github.com/saworbit/SPACE/issues/99)).

- **Every segment is verified.** Encrypted segments are checked by MAC, unencrypted ones by their `content_hash`. A capsule whose policy requires encryption is never served from a segment marked unencrypted, so clearing the sidecar's `encrypted` flag doesn't skip verification.

- **MAC before decrypt.** The MAC tag is verified against the ciphertext *before* decryption. If the ciphertext was tampered with, decryption never runs — we don't process untrusted input through the AES core.

- **Constant-time MAC compare.** Uses `subtle::ConstantTimeEq` to prevent timing side-channels on tag verification.

---

## 7. Deletion, ref counting, and GC

Capsules are deleted by reference; segments are deleted by reaching ref_count zero.

### Ref count lifecycle

```
Segment lifecycle:
  Write new        : ref_count = 1
  Dedup hit        : ref_count += 1
  Capsule deleted  : ref_count -= 1 per reference; at the last reference the segment
                     record is removed and its hash unregistered
  Bytes            : stay in the log until compaction
```

### Delete path

`Pipeline::delete_capsule(capsule_id)` is library-only: neither `spacectl` nor the S3 view calls it ([#86](https://github.com/saworbit/SPACE/issues/86)). It:

1. Looks up the Capsule.
2. For each `SegmentId` in `capsule.segments`: if `ref_count > 1`, decrements it; otherwise deletes the segment record and unregisters its hash.
3. Removes the Capsule record from the registry.

Segments still referenced by other Capsules remain.

### Garbage collection

`Pipeline::garbage_collect()` removes segments that no Capsule references, or whose `ref_count` is 0, and unregisters their hashes. Unreferenced segments are typically left over from a failed or interrupted write.

**Bytes remain in the log** until compaction (`NvramLog::compact()`) rewrites it. The two-step design keeps deletion cheap (touch one record) and bulk reclamation infrequent. Nothing runs GC or compaction automatically yet ([#97](https://github.com/saworbit/SPACE/issues/97)).

### Race-freedom

The classic race is a segment reaching zero just as a new write is about to dedup-hit it. Every pipeline operation on a handle (write, delete, GC) runs under one async mutex, so they never interleave. A second process can't open the same store at all, because sled locks `space.db`.

---

## 8. Scrub: continuous integrity

Capsules are durable only if their bytes still match their hashes and MACs. Scrub is the background loop that proves this. `ScrubExecutor` implements it, but nothing on this branch starts it yet ([#97](https://github.com/saworbit/SPACE/issues/97)).

### Two flavors

| Kind | What it verifies | I/O | CPU |
|---|---|---|---|
| **Light** | Stored byte count matches `segment.len` | Full read | None |
| **Deep** | MAC tag (encrypted) or content hash (unencrypted) | Full read | BLAKE3 / AES |

Both run on configurable intervals (defaults: light = 24h, deep = 7d). The executor tracks each segment's last-checked times in memory, and a cycle scrubs only segments that are due.

### Deep scrub of encrypted segments

1. Read ciphertext from backend.
2. Look up `key_version` → retrieve `XtsKeyPair`.
3. `verify_mac(ciphertext, metadata, key_pair)` — constant-time.
4. Failure → `ScrubResult::MacMismatch` (durable corruption or tampering).

An encrypted segment with no `integrity_tag` is recorded as `MacMismatch`, the same as the read path, which refuses it.

### Deep scrub of unencrypted segments

1. Read bytes from backend.
2. `verify_content_hash(expected_hash, data, algorithm_string)` → `VerifyOutcome::{Matched, LegacyMatched, Mismatched}`.
3. `Matched` → healthy. `Mismatched` → `ContentCorrupted`. `LegacyMatched` → healthy *but* the segment was written before the algorithm-domain-separated hash fix and is verifiable only via the bare BLAKE3 fallback.

The `legacy_hash_hits` counter in `ScrubReport` is the operator's gauge for retiring the compatibility window. When it trends to zero across the store, the legacy fallback can be removed.

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
common           ─ Capsule, Segment, ContentHash, Policy, scrub types, all traits
                   (no I/O, no async runtime, no crypto impl — just types & contracts)

pipeline         ─ Pipeline: the write/read/range/delete/GC implementation.
                   XtsEncryptor, NoopEncryptor.

capsule-registry ─ CapsuleRegistry (sled metadata). WritePipeline and
                   RegistryPipelineHandle wrappers. ScrubExecutor.

storage          ─ StorageBackend implementations: InMemory, Nvram, TokioFs, AutoFs, Cached.
                   Optional io_uring (`uring` feature) on Linux.

nvram-sim        ─ NvramLog: append-only log + JSON sidecar, compaction.

dedup            ─ hash_content, hash_content_with_algo, verify_content_hash.
                   Blake3Deduper.

encryption       ─ XTS-AES-256, BLAKE3 MAC, KeyManager (HKDF), key providers.

compression      ─ LZ4 and Zstd, with entropy-based skipping.

protocol-s3      ─ S3 view (axum). Owns no bytes — it maps bucket/key to capsules.

spacectl         ─ CLI: create, put, read, list, serve-s3.
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
- **Metadata only via transactions.** Direct field-level mutation is intentionally not in the trait surface. `begin_txn().await?.set_segment_metadata(id, seg).await?.commit().await?` is the single path for segment metadata.

### 9.3 Metadata commits

There's no transaction spanning the two stores. A write commits in this order:

1. For each new segment: append the bytes (fsynced), write the segment record (sidecar rewrite, not fsynced), then register its hash in sled.
2. For each dedup hit: write the segment record with the incremented `ref_count`.
3. After the last segment: insert the capsule record into sled, then flush.

A crash before step 3 leaves no visible capsule. It does leave stored segments with no capsule pointing at them, and ref-count increments on shared segments that nothing will ever decrement. GC can clear the unreferenced segments, but not the inflated counts ([#97](https://github.com/saworbit/SPACE/issues/97)). Each individual sled insert is crash-safe.

### 9.4 PODMS swarm behavior (archived)

The PODMS hooks (`apply_transform`, `on_migrate`, `SwarmBehavior`) let a Capsule re-shape itself during migration between zones. They were removed in the reset and are on `archive/pre-reset-2026-10`.

---

## 10. Design rationale

Each subsection below covers one load-bearing decision and the alternative we rejected.

### 10.1 Why 128-bit UUIDs for capsule identity

**Decision**: `CapsuleId` is a 128-bit UUID v4, generated locally.

**Alternative**: monotonic 64-bit ID from a coordinator, or content-hash-as-ID.

**Why**: SPACE has to work across zones, federated clusters, and disconnected edge sites. A coordinator on the write path is unacceptable for an air-gapped edge node. Content-hash-as-ID was tempting but breaks the moment a Capsule is re-encrypted or transcoded — its bytes change, but the *logical* identity must not (see [§2.1](#21-identity-vs-representation)). UUIDs decouple identity from representation, allowing storage-layer transformations to rewrite segments while preserving the references that views, audit logs, and federation peers hold.

128 bits is the standard collision-safe width for uncoordinated generation. 64 bits has a real collision risk at cluster scale (birthday bound ≈ 2³² capsules).

### 10.2 Deterministic encryption preserves dedup

**Decision**: XTS-AES-256 with the tweak taken from a BLAKE3 hash of the segment's compressed bytes (first 16 bytes).

**Alternative**: AES-GCM with a random nonce per segment.

**Why**: GCM with random nonces breaks dedup — encrypting the same plaintext twice produces different ciphertext, so two capsules with identical content store two copies. SPACE chose XTS with a deterministic tweak so that identical plaintext + identical key produces identical ciphertext, preserving dedup *after* encryption.

The price: XTS is technically less robust against chosen-ciphertext attacks than GCM. We pay this back with the BLAKE3-MAC layer ([§10.3](#103-mac-on-top-of-xts)), which gives us authenticated encryption properties at the cost of one extra hash per segment. The combined construction is XTS-then-MAC, not GCM.

The tweak derives from the segment's own bytes, so identical segments get identical tweaks. This is unusual: XTS tweaks are conventionally sector numbers. Using a content-derived tweak means identical segments encrypt identically wherever they're stored, and compaction can move segments to new offsets without re-encrypting them. The cost is that equal segments are visible as equal ciphertext.

### 10.3 MAC on top of XTS

**Decision**: After XTS encryption, compute a BLAKE3-keyed MAC over `ciphertext || serialized_metadata`. Store the first 16 bytes as `integrity_tag`.

**Alternative**: rely on XTS alone, or use AES-GCM (which bundles auth).

**Why**: XTS is a length-preserving cipher with no authentication — corrupting ciphertext produces corrupt plaintext silently. The MAC layer turns this into authenticated encryption. The MAC also covers the encryption metadata (version, key version, tweak, length), so those fields can't be changed undetected. It doesn't bind a segment to a Capsule (dedup deliberately shares segments). It also doesn't cover the sidecar's `encrypted` flag. Clearing that flag doesn't help an attacker: the segment then fails its content-hash check, and a capsule that requires encryption refuses it outright.

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
- **Hash compressed-pre-encryption**: dedup matches stored compressed bytes; the tweak can derive deterministically from the same bytes; encryption preserves dedup. This is the only point in the pipeline where all three properties align.

### 10.6 Atomic metadata via `.tmp` + rename

**Decision**: `NvramLog::save_segment_map` writes the new sidecar to `{path}.segments.tmp`, then atomic-renames over the live file.

**Alternative**: overwrite in place.

**Why**: a crash mid-overwrite leaves the sidecar truncated and unparseable, taking the entire segment map down. With `.tmp` + rename, a crash before rename leaves the old sidecar intact; a crash after rename leaves the new sidecar intact. The window between is the kernel's rename atomicity, which on POSIX is a single inode flip. On Windows, `std::fs::rename` replaces the destination the same way; the code also falls back to remove-then-rename if the rename reports that the target exists. Neither file is fsynced around the rename ([#96](https://github.com/saworbit/SPACE/issues/96)).

The `.compacting` marker covers a different failure: a crash mid-compaction, while the segment log itself is being rewritten. If the marker exists, `open()` refuses to open the log. An operator then has to verify or restore the data and delete the marker ([#108](https://github.com/saworbit/SPACE/issues/108)).

### 10.7 4 MiB default segment size

**Decision**: `SEGMENT_SIZE = 4 * 1024 * 1024`.

**Alternative**: smaller (better dedup granularity) or larger (less per-segment overhead).

**Why**: 4 MiB is the empirical sweet spot across the workloads SPACE targets:
- Large enough that per-segment metadata (about 490 bytes in the JSON sidecar) is about 0.01% of the segment.
- Small enough that range reads waste at most 4 MiB.
- Small enough that dedup hits are common for many real-world files (video frames, container layers, source archives).
- Aligned with common SSD erase block and io_uring submission sizes.

`LayoutPolicy` can change the size (`Fixed`, `ZnsGraph`); the other strategies fall back to 4 MiB. Within a Capsule every segment is the same size except the last.

### 10.8 Append-only log as primary store

**Decision**: NVRAM is append-only; modifications happen via new segments and ref count changes; reclamation happens via compaction.

**Alternative**: in-place update with a free list.

**Why**: append-only matches NVRAM/flash physics (no rewrites = no write amplification = longer device life), matches replication semantics (ship the log forward), and dramatically simplifies crash recovery (no partial-write torn states). The intent is for compaction to be amortized in the background. Today nothing schedules it, and when it does run it holds the log's write lock, so it blocks writes ([#97](https://github.com/saworbit/SPACE/issues/97), [#108](https://github.com/saworbit/SPACE/issues/108)).

The trade-off is space overhead: GC'd segments occupy bytes until the next compaction. For workloads with rapid churn this is real. `compact_interval_secs` is meant to tune the frequency, but nothing reads it yet.

### 10.9 Two-store split: NVRAM log + Sled registry

**Decision**: bytes in NVRAM log, metadata in Sled.

**Alternative**: a single store for both.

**Why**: the access patterns are fundamentally different. Segment bytes want streaming sequential I/O, large blocks, and append semantics. Capsule metadata wants point lookups, transactional updates, and small records. Fusing them means either the metadata path is slow (full-log scans) or the byte path is slow (random-write database pages).

Sled gives crash-safe point updates for metadata. NVRAM log gives append throughput for bytes. The handoff between the two is the one place where atomicity matters, and it isn't atomic yet; see [§9.3](#93-metadata-commits).

### 10.10 Trust segment metadata over policy on read

**Decision**: read-path branches on `segment.compressed`, `segment.encrypted`, `segment.compression_algo`. Never on `capsule.policy`.

**Alternative**: read `capsule.policy` and infer how to decode.

**Why**: a Capsule's `policy` field reflects the *current* intended representation, but transformation is not atomic across all segments — during a migration, key rotation, or transcoding sweep, segments may be heterogeneous (some already rewritten, some not). Even at rest, key rotation can leave segments at different `key_version`s. Branching on the Capsule-level policy would silently corrupt reads any time stored segments diverge from it. Branching on segment metadata is always correct because that's what's actually on disk.

---

## 11. Invariants

These are the invariants the system aims to maintain. The first four are tested by the proptest harness in [crates/capsule-registry/tests/proptest_pipeline.rs](../crates/capsule-registry/tests/proptest_pipeline.rs). The rest aren't tested; where one currently breaks, that's noted under it.

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
Broken by a crash mid-write: a dedup hit's increment survives even though its capsule was never created ([#97](https://github.com/saworbit/SPACE/issues/97)).

### Crash recovery

After a crash during a write:
```
open() succeeds
all committed Capsules remain readable
no Capsule is partially visible
```
Checked by killing `spacectl create` mid-write: this holds, but the interrupted write's segments stay in the log ([#97](https://github.com/saworbit/SPACE/issues/97)). It does not hold after a crash during compaction, where `open()` refuses to start ([#108](https://github.com/saworbit/SPACE/issues/108)), or if the sidecar is lost ([#96](https://github.com/saworbit/SPACE/issues/96)).

### Segment metadata is truth

```
read_path branches on segment.{compressed, encrypted, compression_algo, key_version}
never on capsule.policy
```

---

## 12. Failure modes and recovery

| Failure | Detection | Recovery |
|---|---|---|
| **Bit rot in segment bytes** | Deep scrub: content hash or MAC mismatch. Reads catch it too. | `ScrubResult::ContentCorrupted` / `MacMismatch`. Restore from backup or accept the loss (no replicas on this branch). |
| **Truncated segment file** | Light scrub: actual_len ≠ metadata.len | `ScrubResult::MetadataMismatch`. Same as above. |
| **Lost sidecar** | `open()` starts with an empty map; reads fail with `Segment not found` | None. The log has no framing to rebuild from ([#96](https://github.com/saworbit/SPACE/issues/96)). |
| **Crash mid-`save_segment_map`** | `.segments.tmp` exists at `open()` | If `.segments` still exists, the stale `.tmp` is ignored and left in place. If `.segments` is missing, the `.tmp` is promoted. |
| **Crash mid-compaction** | `.segments.compacting` marker present | `open()` refuses to open the log. Verify or restore the data, then delete the marker ([#108](https://github.com/saworbit/SPACE/issues/108)). |
| **Crash mid-append** | None; `open()` appends after the current end of the file | Nothing truncates the log. The partial bytes, and any segments the interrupted write had already stored, stay until GC and compaction run ([#97](https://github.com/saworbit/SPACE/issues/97)). |
| **Sled corruption** | Sled checksum failure on read | Restore `space.db` from backup. There's no audit log to replay. |
| **Wrong key on read** | MAC verify fails | Read fails with auth error. No plaintext is exposed. |
| **Key version no longer wanted** | n/a: every version is re-derived from the master key on demand | Old versions can't be retired while the master key is kept, and losing the master key loses every version. |
| **Dedup-hit-then-GC race** | Cannot occur: write, delete and GC run under the pipeline handle's lock | n/a |
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
| **Policy** | The compression/encryption/layout bundle a Capsule was written with. Set at write time; see [§3.6](#36-policy). |
| **Integrity tag** | First 16 bytes of `BLAKE3_keyed(mac_key, ciphertext ‖ metadata)`. The MAC. |
| **Tweak** | 16-byte XTS tweak: the first 16 bytes of a BLAKE3 hash of the segment's compressed bytes. |
| **Light scrub** | Background verification of stored byte counts. |
| **Deep scrub** | Background verification of MAC tags (encrypted) or content hashes (unencrypted). |
| **Legacy hash hit** | A segment that verifies only under the pre-fix bare BLAKE3 hash; tracked in `ScrubReport.legacy_hash_hits`. |
| **Backend** | An implementation of `StorageBackend` — where segment bytes physically live. |
| **Registry** | The Sled-backed metadata store: Capsules, content-hash index, segment ID counter. |
| **NVRAM log** | The primary durable backend. Append-only log + JSON sidecar. |
| **Compaction** | Rewriting the live segments of an NVRAM log to reclaim space from GC'd segments. |
| **PODMS** | Policy-Orchestrated Disaggregated Mesh Scaling. The distributed mode, archived in the reset. |
| **Sovereignty** | A PODMS policy attribute controlling whether a Capsule may be replicated/migrated across zones (archived). |

---

## 14. Cross-references

### Source code

- Core types: [crates/common/src/lib.rs](../crates/common/src/lib.rs)
- Policy: [crates/common/src/policy.rs](../crates/common/src/policy.rs)
- Traits: [crates/common/src/traits.rs](../crates/common/src/traits.rs)
- Scrub types: [crates/common/src/scrub.rs](../crates/common/src/scrub.rs)
- Write/read pipeline: [crates/pipeline/src/lib.rs](../crates/pipeline/src/lib.rs) (wrapped by [crates/capsule-registry/src/pipeline/](../crates/capsule-registry/src/pipeline/))
- Scrub executor: [crates/capsule-registry/src/scrub_executor.rs](../crates/capsule-registry/src/scrub_executor.rs)
- GC: [crates/capsule-registry/src/gc.rs](../crates/capsule-registry/src/gc.rs)
- NVRAM log: [crates/nvram-sim/src/lib.rs](../crates/nvram-sim/src/lib.rs)
- Dedup: [crates/dedup/src/lib.rs](../crates/dedup/src/lib.rs)
- Encryption: [crates/encryption/src/](../crates/encryption/src/)
- Proptest invariants: [crates/capsule-registry/tests/proptest_pipeline.rs](../crates/capsule-registry/tests/proptest_pipeline.rs)

### Related documentation

- [ARCHITECTURE.md](../ARCHITECTURE.md): the single-node core at a glance.
- [S3 quick start](guides/QUICKSTART_S3.md) and [Streaming reads](guides/STREAMING_READS.md).
- [Encryption Implementation](implementation/ENCRYPTION_IMPLEMENTATION.md): XTS-AES-256 + BLAKE3-MAC deep dive.
- [Dedup Implementation](implementation/DEDUP_IMPLEMENTATION.md): content index, ref counting, GC.
- [Encrypted Dedup Flow](implementation/ENCRYPTED_DEDUP_FLOW.md): how the two fit together.
- [Dependency Security](dependency-security.md): crypto dependency policy.
- The pre-reset design docs (architecture, CapsuleFlow layout engine, protocol views, PODMS, future-state architecture) are on `archive/pre-reset-2026-10`.

---

*Last updated: 2026-10-29. If you change any invariant in §11, update both the corresponding proptest and this document.*
