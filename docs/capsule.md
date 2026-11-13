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

- **Protocol-layer mutations** (a client overwriting a file at `/foo.txt`, rewriting block offset 4096 of `vol0`, PUT-ing a new object body to `s3://bucket/key`) produce a **new** Capsule with a new `CapsuleId`. The view's metadata is updated to point at the new Capsule; the old one is dereferenced and may be GC'd. The S3 view deletes the old capsule as soon as the key points at the new one. Its bytes stay in the log until compaction. The Capsule abstraction is *not* in-place mutable from the protocol surface.
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
| `plain_len` | Write pipeline (segment size before compression) | Range-read planner (skip segments outside range without decompressing) |
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
- `spacectl create` and S3 PUT use `Policy::default()`, with encryption turned on when `SPACE_MASTER_KEY` is set. `spacectl create --policy-file` takes a YAML policy instead.

### 3.7 `Event`

The `Event` enum defines audit events: `CapsuleCreated`, `CapsuleRead`, `CapsuleDeleted`, `SegmentAppended`, `DedupHit`, `AuditHeartbeat`. Nothing on this branch emits them, and there's no audit log.

---

## 4. Where Capsules live

### 4.1 The two-store model

A Capsule's existence is split across two persistence layers:

| Store | Holds | Format | Crate |
|---|---|---|---|
| **NVRAM log** | Segment bytes + segment metadata | Append-only log of framed records ([on-disk format](on-disk-format.md)) | [`nvram-sim`](../crates/nvram-sim/) |
| **Capsule registry** | Capsule records + content hash index | Sled (embedded KV) | [`capsule-registry`](../crates/capsule-registry/) |

The split is deliberate: segment bytes are bulky and want streaming I/O; capsule metadata is small and wants point lookups + transactions. Fusing them would mean either slow lookups or slow appends.

### 4.2 NVRAM log layout

The NVRAM log is the **primary** durable store. Segments are appended and nothing is rewritten in place: compaction writes a new file and renames it over the old one.

```
{path}                          # On-disk format 1 data log
  ├─ FILE record                  format version, store id, generation, segment id floor
  ├─ SEGMENT record               48-byte frame + descriptor + stored bytes
  ├─ SEGMENT record
  └─ ...

{path}.compact                  # The next generation, while compact() writes it
```

Each `SEGMENT` record carries the segment's write-once descriptor (compression, content hash, encryption fields) next to its bytes, with checksums over both. The byte layouts are in [on-disk-format.md](on-disk-format.md) §4–§5.

**Opening and crash safety**:
- There is no sidecar. `open()` scans the log and rebuilds the segment map from the records.
- A record cut short at the end of the file, from an append a crash interrupted, fails its checksums with nothing valid after it. `open()` truncates it away. That write never committed: segments are fsynced before their hashes are registered or a capsule refers to them.
- Bytes that hold no valid record but have valid records after them are damage. The log then opens read-only, and `NvramLog::damage()` lists the ranges. Records outside them still read.
- Only the last record's body is checksummed on open; earlier bodies are verified when read (MAC or content hash) and by scrub, which checks every record's checksum.
- Ref counts aren't stored. They're held in memory and recounted from the registry's capsules whenever a pipeline opens the log.
- If `{path}.compact` exists, a compaction stopped before its rename. `open()` deletes it; the log it would have replaced is intact.
- If `{path}.compacting` exists, `open()` refuses to open the log. Only the in-place compaction of earlier builds wrote that marker.
- A log written before format 1 (with a `.segments` sidecar) is refused; see [on-disk-format.md](on-disk-format.md) §13.

**Compaction** (`NvramLog::compact()`) reclaims bytes from deleted and orphaned segments by writing the next generation of the log ([on-disk-format.md](on-disk-format.md) §9.1):

1. It notes the live records: segments with a ref count above 0, which includes segments an open write holds and those a GET still reads after their capsule was dropped.
2. Without holding the log's locks, it writes `{path}.compact`: a `FILE` record with the generation incremented, then each live record copied unchanged, a buffer at a time. Reads and appends carry on meanwhile.
3. Under the log's locks, it copies the records appended since step 1, fsyncs, and renames the new file over the log.

A crash before the rename leaves the old log; one after it leaves the new generation. Both open without manual steps. A log with nothing to reclaim is left alone. Compaction refuses to run until ref counts have been recounted, so it can't mistake a live segment for a dead one. The new `FILE` record keeps the segment id floor above every id the log has held, so ids compaction removed are never handed out again. `serve-s3` schedules compaction and `spacectl compact` runs it on demand ([§Garbage collection](#garbage-collection)).

### 4.3 Capsule registry

The registry has two parts:

- **The capsule journal**, `space.journal` next to `space.db`. An append-only file of framed records: a `CAPSULE` manifest commits each write, a `DROP` commits each delete, and `NAMESPACE`, `BIND` and `UNBIND` records create namespaces and point names at capsules ([on-disk-format.md](on-disk-format.md) §5–§7). Several records can commit together as one `BATCH`. It's the durable record of which capsules and names exist.
- **A sled index of the journal**, in `space.db`:
  - `Capsule` records keyed by `CapsuleId`
  - the content-hash index, `ContentHash → SegmentId`, for dedup lookup
  - per-segment reference counts, and which segment each indexed hash belongs to
  - namespaces, and the capsule each name points at
  - the journal position the index has reached
  - a counter for allocating `SegmentId`s

Each journal record, or each whole `BATCH`, is applied to the index in one sled transaction that also records the new position. On open, records past that position are replayed; an index that doesn't match the journal, or a missing `space.db`, is rebuilt from it.

**Journal compaction** (`CapsuleRegistry::compact_journal()`) stops the journal growing without bound under deletes and overwrites ([on-disk-format.md](on-disk-format.md) §9.2). It writes the live capsules, namespaces and names to `space.journal.compact`, the journal's next generation, while commits carry on. Then, under the journal lock, it copies the records committed meanwhile, renames the new file over the journal, and moves the index's position to the new generation. A crash before the rename leaves the old journal, and open deletes the unfinished file. A crash after it leaves the new generation, and open rebuilds the index from it. `serve-s3`'s reclaim pass compacts the journal once a quarter of it, and at least 1 MiB, is dead, and `spacectl compact` compacts it whenever anything in it is.

Names are opaque strings to the registry. The S3 view keeps each bucket as the namespace `s3:<bucket>` and each key as a name in it.

Segment metadata isn't in the registry: the descriptor is written once, in the segment's record in the NVRAM log, through `StorageBackend::begin_txn()…commit()`, and the log's in-memory ref counts are set from the index when a pipeline opens. See [§9.3](#93-metadata-commits) for the commit order.

### 4.4 Storage backends

`StorageBackend` is the trait that abstracts where segment bytes physically live. Current implementations:

| Backend | Crate | Use case |
|---|---|---|
| `InMemoryBackend` | `storage` | Tests; ephemeral workloads. `Arc<Mutex<Inner>>` — clones share state. |
| `NvramBackend` | `storage` (wraps `nvram_sim::NvramLog`) | Primary durable store; append-only log on disk. |
| `TokioFsBackend` | `storage` | A storage root: `root/store` holds the store id, and `root/segments/<id>.seg` holds one framed `SEGMENT` record per segment ([on-disk-format.md](on-disk-format.md) §10), written to a `.tmp` file, fsynced and renamed into place. Ref counts are kept in memory and recounted when a pipeline opens. |
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
│ 8. Capsule      │  journal: append CAPSULE manifest, fsync; index applies it
└─────────────────┘
```

### Stage-by-stage

**1. Segmentation.** The write pipeline splits the input into chunks. Default chunk size is 4 MiB (`SEGMENT_SIZE`). `LayoutPolicy` can set another size, either `Fixed { segment_size }` or a zone size with `ZnsGraph { zone_size_mib }`. `AdaptiveEntropy`, `Learned` and `QuantumReady` all fall back to 4 MiB, so there's no variable-size chunking. `Policy::validate` rejects a size of 0 or above 16 MiB (`MAX_SEGMENT_SIZE`), which keeps each segment within the XTS data unit limit.

**2. Compression.** Driven by `policy.compression`. Returns both the compressed bytes and the **algorithm string** (e.g., `"lz4:1"`, `"zstd:3"`, `"identity"`). The algorithm string is critical for step 3.

**3. Hash.** `hash_content_with_algo(compressed_bytes, algorithm_string)` produces a `ContentHash` that mixes the algorithm into the BLAKE3 input. This prevents cross-policy dedup collisions ([§10.4](#104-dedup-keys-need-algorithm-domain-separation)).

**4. Dedup check.** The registry looks up the content hash. On hit, the existing `SegmentId` is reused — no further work, just `ref_count += 1` on the existing segment and append the SegmentId to the new Capsule's segment list. On miss, proceed. The hash domain includes the encryption mode, so encrypted and plaintext writes never share a segment. With `policy.dedupe = false` the lookup is skipped.

**5. Encrypt.** Runs only when the policy enables encryption. XTS-AES-256, with the tweak taken from the first 16 bytes of a plain BLAKE3 hash of the compressed segment (not the algorithm-separated `content_hash`). Encryption is deterministic: the same compressed bytes under the same key version give the same ciphertext. This is the property that lets encryption preserve dedup ([§10.2](#102-deterministic-encryption-preserves-dedup)). XTS needs at least 16 bytes, so a shorter segment is stored uncompressed and zero-padded to 16 bytes. The read strips the padding using `plain_len`.

**6. MAC.** BLAKE3-keyed MAC over the ciphertext plus the encryption metadata (encryption version, key version, tweak, ciphertext length). The MAC key is derived from both XTS subkeys. The first 16 bytes of the keyed hash become the `integrity_tag`.

**7. Append.** The segment's record (descriptor and bytes) is appended to the NVRAM log through a storage transaction and fsynced. Its content hash goes into the registry's in-memory pending index, where other writes can dedup against it; it reaches the durable index only when its capsule commits.

**8. Capsule.** After the last segment, the pipeline builds the capsule's manifest: its id, size, a BLAKE3 digest of the logical bytes (computed as the chunks went by), flags, the write-time policy, and one entry per segment with its plain length and content hash. Appending that `CAPSULE` record to the journal and fsyncing it commits the write; the index then applies it.

### Important properties of the write path

- **Determinism**: write the same bytes under the same policy twice → same content hash → dedup hit on the second write. This is observable as `Capsule.deduped_bytes > 0` in the second Capsule.
- **No partially visible capsules**: the `CAPSULE` record is appended only after every segment is stored, so a failed or interrupted write never shows up in `list` or `read`. Any segments it had already stored stay behind, unreferenced and out of the content index, until compaction runs ([#97](https://github.com/saworbit/SPACE/issues/97)).
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

- **Range reads skip work.** With `segment.plain_len` set, `read_range` skips segments that end before the requested range. A segment that overlaps the range is still read, decrypted and decompressed in full, so a 4 KiB range costs one or two segment reads (up to 8 MiB), not 4 KiB. S3 GET uses them for `Range` requests.

- **Every segment is verified.** Encrypted segments are checked by MAC, unencrypted ones by their `content_hash`. A capsule whose policy requires encryption is never served from a segment marked unencrypted, so clearing a segment's `encrypted` flag doesn't skip verification.

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

`Pipeline::delete_capsule(capsule_id)` runs for `spacectl delete`. It:

1. Commits the delete: appends a `DROP` record to the journal and fsyncs it. Applying it removes the capsule record, decrements each segment's reference count in the index, and removes a segment's hash from the content index when its count reaches zero.
2. Releases the capsule's segments: for each `SegmentId` in `capsule.segments`, if the in-memory `ref_count` is above 1, decrements it; otherwise drops the segment from the log's map.

The delete commits first, so a crash part way through step 2 leaves only in-memory counts, which the next open sets again from the index.

S3 DELETE and overwrites commit the `DROP` in the same `BATCH` as the key change (`Pipeline::finish_write_with` or `Pipeline::commit`), then release the dropped capsule with `Pipeline::release`. If a GET is still reading it, the S3 view holds the release until the GET ends: the in-memory references pin the segments, so they stay readable.

Segments still referenced by other Capsules remain. `spacectl delete` leaves any S3 key that points at the capsule in place; the key then reads as missing ([#138](https://github.com/saworbit/SPACE/issues/138)).

### Garbage collection

`Pipeline::garbage_collect()` removes segments that no Capsule references and nothing pins, and unregisters their hashes. They're typically left over from a failed or interrupted write. Pinned segments are kept: those an open write holds, and those of a Capsule a commit dropped until `Pipeline::release` is called for it. That covers an S3 GET still reading an object an overwrite or DELETE removed. A segment a Capsule references is kept whatever its stored `ref_count` says.

**Bytes remain in the log** until compaction (`NvramLog::compact()`) writes the next generation without them. The two-step design keeps deletion cheap (touch one record) and bulk reclamation infrequent. In the NVRAM log, compaction alone reclaims unreferenced segments, since they have no references in the log's counts. Under `SPACE_STORAGE_ROOT` each segment is a file, and garbage collection deletes it.

**Scheduling.** `serve-s3` runs a reclaim pass at startup and then every `--reclaim-interval` seconds (default 300; 0 turns it off):

- On the NVRAM log it compacts once a quarter of the log, and at least one default segment (4 MiB), is reclaimable, on a blocking thread and without the pipeline lock, so requests carry on.
- Under `SPACE_STORAGE_ROOT` it garbage-collects, with the pipeline lock held.
- On either, it then compacts the capsule journal once a quarter of it, and at least 1 MiB, is dead, also without the pipeline lock ([§4.3](#43-capsule-registry)).

`spacectl gc` and `spacectl compact` run the steps on demand, with the server stopped, since only one process can open a store. `spacectl compact` compacts both `space.nvram` and `space.journal`.

### Race-freedom

The classic race is a segment reaching zero just as a new write is about to dedup-hit it. Every pipeline operation on a handle runs under one async mutex. A streaming write takes it once per segment, so other writes and deletes can run between its segments. Each segment a write uses holds a reference from the moment it is stored, and garbage collection keeps every segment an open write pins, as well as those of a dropped capsule that hasn't been released yet. A second process can't open the same store at all, because sled locks `space.db`.

---

## 8. Scrub: continuous integrity

Capsules are durable only if their bytes still match their hashes and MACs. Scrub is the background loop that proves this. `ScrubExecutor` implements it. `serve-s3` runs it, and `spacectl scrub` runs one pass on demand.

### Two flavors

| Kind | What it verifies | I/O | CPU |
|---|---|---|---|
| **Light** | In `space.nvram` and in a storage root's segment files, each record's own checksum (`body_sum` and `head_sum`). In `InMemoryBackend`, that the stored byte count matches `segment.len`. | Full read | BLAKE3 |
| **Deep** | MAC tag (encrypted) or content hash (unencrypted) | Full read | BLAKE3 / AES |

Light scrub needs no key. In the data log and under `SPACE_STORAGE_ROOT` it catches media damage in any segment, encrypted or not ([#135](https://github.com/saworbit/SPACE/issues/135)). A segment file whose header is damaged has no readable metadata, and both scrubs report it as corrupted rather than unreadable. Deep scrub also catches tampering, which the unkeyed record checksums can't, but it needs `SPACE_MASTER_KEY` for encrypted segments and skips them without it.

The executor tracks when each segment was last checked, and a cycle scrubs only segments that are due. `serve-s3` and `spacectl scrub` keep those times in `space.db`, in the `scrub_schedule` tree, so a restart picks up where the last run left off; losing them costs one full scrub. It forgets segments that have been deleted. A cycle takes at most `max_segments_per_cycle` segments (1024 by default), and the rest follow a second later rather than an interval later. A segment deleted while a cycle runs isn't reported.

### Running it

`serve-s3` scrubs in the background, without the pipeline lock:

| Flag | Default | Meaning |
|---|---|---|
| `--scrub-interval` | 86400 (1 day) | Seconds between light scrubs. `0` turns scrubbing off. |
| `--deep-scrub-interval` | 604800 (1 week) | Seconds between deep scrubs. |
| `--scrub-rate` | 64 | MiB per second scrubbing reads at most. `0` means no limit. |

The first cycle, at startup, is deep, and checks the segments written or fallen due since the last run ([#134](https://github.com/saworbit/SPACE/issues/134)). After each cycle, the next one is timed for when the next segment falls due, plus a 64th of the interval so segments due close together share a cycle, rather than a whole interval later. A cycle that checked segments logs its outcome at `INFO`, or at `WARN` with counts when it finds damage.

`spacectl scrub` checks every segment once, unthrottled, and `spacectl scrub --deep` runs the deep checks. Each failed segment is printed with the capsules that hold it, and the command exits non-zero when any segment fails.

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
Idle → Running(kind) → Completed { kind, errors } → Running(kind) → …
```

`Completed` stays the current value until the next cycle starts.

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

nvram-sim        ─ NvramLog: append-only log of framed records, scan on open, compaction.

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

1. For each new segment: append its record (descriptor and bytes) and fsync, then put its hash in the in-memory pending index.
2. For each dedup hit: increment the segment's ref count in memory. Nothing is written.
3. After the last segment: append the `CAPSULE` record to the journal and fsync it. This is the commit point.
4. Apply the record to the sled index in one transaction: the capsule record, reference counts, content-index entries for its segments, and the new journal position.

A crash before step 3 leaves no visible capsule, and nothing in the durable content index points at its segments. They come back with no references, and compaction reclaims them: at a `serve-s3` reclaim pass, or with `spacectl compact`. A crash between steps 3 and 4 is repaired on the next open, which replays the journal past the index's position. The log's in-memory ref counts are set from the index whenever a pipeline opens, so no crash leaves a count wrong.

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

**Why**: XTS is a length-preserving cipher with no authentication — corrupting ciphertext produces corrupt plaintext silently. The MAC layer turns this into authenticated encryption. The MAC also covers the encryption metadata (version, key version, tweak, length), so those fields can't be changed undetected. It doesn't bind a segment to a Capsule (dedup deliberately shares segments). It also doesn't cover the `encrypted` flag in the segment's descriptor, which only a checksum protects. Clearing that flag doesn't help an attacker: the segment then fails its content-hash check, and a capsule that requires encryption refuses it outright.

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

### 10.6 Self-describing log records

**Decision**: every segment's record carries its descriptor next to its bytes, with checksums over both. `open()` rebuilds the segment map by scanning the log.

**Alternative**: a separate metadata file. Until on-disk format 1 that was a JSON sidecar, `{path}.segments`, rewritten through `.tmp` + rename on every append and every ref-count change.

**Why**: the sidecar was the only map of the data. Losing or corrupting that one file made every segment unreadable, and it wasn't fsynced, so after a power loss it could lag behind the data ([#96](https://github.com/saworbit/SPACE/issues/96)). It also grew with the store and was rewritten whole on every change. The descriptor never changes after a write once ref counts are derived instead of stored, so it can sit in the record and be written exactly once. See [on-disk-format.md](on-disk-format.md) §2.

Compaction doesn't rewrite the log either. It writes the next generation to a new file and renames it into place, so a crash leaves either the old log or the new one, and no marker or manual step is needed ([#108](https://github.com/saworbit/SPACE/issues/108)).

### 10.7 4 MiB default segment size

**Decision**: `SEGMENT_SIZE = 4 * 1024 * 1024`.

**Alternative**: smaller (better dedup granularity) or larger (less per-segment overhead).

**Why**: 4 MiB is the empirical sweet spot across the workloads SPACE targets:
- Large enough that per-segment metadata (a 48-byte frame and a descriptor of about 90 bytes) is under 0.01% of the segment.
- Small enough that range reads waste at most 4 MiB.
- Small enough that dedup hits are common for many real-world files (video frames, container layers, source archives).
- Aligned with common SSD erase block and io_uring submission sizes.

`LayoutPolicy` can change the size (`Fixed`, `ZnsGraph`); the other strategies fall back to 4 MiB. Within a Capsule every segment is the same size except the last.

### 10.8 Append-only log as primary store

**Decision**: NVRAM is append-only; modifications happen via new segments and ref count changes; reclamation happens via compaction.

**Alternative**: in-place update with a free list.

**Why**: append-only matches NVRAM/flash physics (no rewrites = no write amplification = longer device life), matches replication semantics (ship the log forward), and dramatically simplifies crash recovery (no partial-write torn states). The intent is for compaction to be amortized in the background. It copies outside the log's locks and holds them only to catch up on appends and rename, and `serve-s3` runs it once a quarter of the log is reclaimable.

The trade-off is space overhead: GC'd segments occupy bytes until the next compaction. For workloads with rapid churn this is real. `serve-s3 --reclaim-interval` sets how often a pass checks, and compaction waits until a quarter of the log is reclaimable. The per-capsule `compact_interval_secs` is still read by nothing: how often to compact is a property of the log, not of a capsule.

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

These are the invariants the system aims to maintain. The first four are tested by the proptest harness in [crates/capsule-registry/tests/proptest_pipeline.rs](../crates/capsule-registry/tests/proptest_pipeline.rs). Ref count consistency and crash recovery have tests in [data_log_test.rs](../crates/capsule-registry/tests/data_log_test.rs) and the `nvram-sim` unit tests. The rest aren't tested; where one currently breaks, that's noted under it.

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
Holds across restarts: ref counts aren't stored, and opening a pipeline sets them from the index's per-segment counts, which only committed capsules contribute to, so a dedup hit from a write that never finished leaves no extra reference. Tested by `an_interrupted_dedup_write_leaves_no_extra_reference` in [data_log_test.rs](../crates/capsule-registry/tests/data_log_test.rs).

### Index rebuild

```
rebuild(journal) == index
```
Deleting `space.db` and reopening gives the same capsules, reference counts and content index, and a stale index catches up by replaying the journal past its position. Tested by `losing_the_index_rebuilds_it_from_the_journal` and `records_the_index_missed_are_replayed_on_open` in [journal_test.rs](../crates/capsule-registry/tests/journal_test.rs). It holds after journal compaction too: `compaction_keeps_only_the_live_state` and `records_committed_while_compaction_runs_are_kept` in [journal.rs](../crates/capsule-registry/src/journal.rs) rebuild the index from a compacted journal, and compare its capsules, reference counts, content index, namespaces and names with the index before.

### Crash recovery

After a crash during a write:
```
open() succeeds
all committed Capsules remain readable
no Capsule is partially visible
```
Checked by killing `spacectl create` mid-write: this holds, but the interrupted write's segments stay in the log until compaction ([#97](https://github.com/saworbit/SPACE/issues/97)). A record the crash cut short is truncated away on open; the nvram-sim tests cut the last record at every byte offset. It holds after a crash during compaction too: the nvram-sim tests stop a data log compaction at each step and reopen the log, and `a_crash_at_any_step_of_compaction_opens_with_the_same_state` does the same for the journal.

### Segment metadata is truth

```
read_path branches on segment.{compressed, encrypted, compression_algo, key_version}
never on capsule.policy
```

---

## 12. Failure modes and recovery

| Failure | Detection | Recovery |
|---|---|---|
| **Bit rot in segment bytes** | Light scrub: the record's checksum (in `space.nvram`). Deep scrub: content hash or MAC mismatch. Reads catch it too. | `ScrubResult::ContentCorrupted` / `MacMismatch`; `spacectl scrub` names the capsules affected. Restore from backup or accept the loss (no replicas on this branch). |
| **Truncated segment file** | Light scrub: actual_len ≠ metadata.len | `ScrubResult::MetadataMismatch`. Same as above. |
| **Damaged bytes inside the log** | `open()` finds bytes that aren't a valid record, with valid records after them | The log opens read-only and `NvramLog::damage()` lists the ranges. Segments outside them still read; capsules with a segment inside can't be read. |
| **Crash mid-compaction** | `{path}.compact` present, or the log is a new generation | Nothing to do. Before the rename, `open()` deletes `{path}.compact` and the old log is intact; after it, the new generation is complete. |
| **Marker from an earlier build** | `{path}.compacting` present | Left by the in-place compaction builds before generation swap used. `open()` refuses; verify or restore the data, then delete the marker. |
| **Crash mid-append** | `open()` finds a last record that fails its checksums, with nothing valid after it | Truncated away; it belonged to a write that never committed. Segments the interrupted write had already stored stay, with no references, until compaction ([#97](https://github.com/saworbit/SPACE/issues/97)). |
| **Lost or corrupt `space.db`** | `space.db` missing, or sled fails to open it | Delete it and reopen: the index, S3 buckets and keys included, is rebuilt from `space.journal`. |
| **Journal record cut short** | `open()` finds a last journal record that fails its checks | Truncated away; it was a commit that never completed, so its write or delete didn't happen. |
| **Damage inside the journal** | Invalid bytes with valid journal records after them | The registry refuses to open: the capsules recorded in the damaged range can't be recovered from it. |
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
| **Light scrub** | Background verification without a key: each record's checksum in the data log, stored byte counts elsewhere. |
| **Deep scrub** | Background verification of MAC tags (encrypted) or content hashes (unencrypted). |
| **Legacy hash hit** | A segment that verifies only under the pre-fix bare BLAKE3 hash; tracked in `ScrubReport.legacy_hash_hits`. |
| **Backend** | An implementation of `StorageBackend` — where segment bytes physically live. |
| **Registry** | The capsule journal and its sled index: capsules, names, content-hash index, reference counts, segment ID counter. |
| **Capsule journal** | `space.journal`: the append-only record of capsule writes (`CAPSULE`) and deletes (`DROP`). The sled index is rebuilt from it. |
| **NVRAM log** | The primary durable backend. Append-only log of framed segment records. |
| **Compaction** | Copying the live segments of an NVRAM log into its next generation, to reclaim space from GC'd segments. |
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
