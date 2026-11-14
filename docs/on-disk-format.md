# On-disk format (draft)

> **Status: draft, partly implemented.** The data log (§4, `FILE` and `SEGMENT` records, scan-on-open and torn-tail handling from §8.1–§8.2) is in `common::format` and `nvram-sim`. The capsule journal (`CAPSULE`, `DROP`, `NAMESPACE`, `BIND`, `UNBIND`, `BATCH`, `KEY`, §5.3–§5.7, §5.9 and §7) is in `capsule-registry`, and the S3 view keeps its buckets and keys there. Data log compaction by generation swap (§9.1) is in `nvram-sim`, and `serve-s3` schedules it along with scrub. Journal compaction (§9.2) is in `capsule-registry`, and runs in the same reclaim pass. Storage roots (§10) keep one `SEGMENT` record per file, in `storage`. The checkpoint is not implemented. §14 lists where the code differs from this draft. It proposes format 1 of the store: a framed segment log plus a capsule journal. It targets [#96](https://github.com/saworbit/SPACE/issues/96) (losing the sidecar loses everything), [#97](https://github.com/saworbit/SPACE/issues/97) (crashes leak segments and inflate ref counts) and [#108](https://github.com/saworbit/SPACE/issues/108) (compaction isn't crash-safe). Stores written by the current code won't open under it (§13).

---

## 1. Goals

- **Everything durable can be rebuilt by reading append-only files.** Losing `space.db` or the segment sidecar loses no data.
- **Nothing is updated in place.** A write appends records and a delete appends a record. Only compaction rewrites, and it does that by writing a new file.
- **Each operation has one commit point**, a single record.
- **Torn writes at the end of a file are detected and dropped without operator steps.** Corruption in the middle of a file is detected and reported.
- **This document defines the bytes.** Record layouts are fixed little-endian structures, not whatever a serde derive emits, so the format doesn't change when a Rust struct does. (`store.rs` already carries legacy bincode decoders for exactly that reason.)

Not goals for format 1: aligned records for O_DIRECT, multi-node anything, an export bundle (it falls out later: a manifest plus its segment records), and moving view metadata such as content type into the capsule.

## 2. What is truth and what is derived

| Fact | Today | Format 1 |
|---|---|---|
| Segment bytes | `space.nvram` | Body of a `SEGMENT` record in the data log |
| Segment descriptor (compression, content hash, crypto fields) | Sidecar JSON, rewritten on every append and every ref-count change | Header of the same `SEGMENT` record, written once |
| Segment offset | Sidecar | Position of the record in the data log |
| Segment ref count | Sidecar, updated in place on every dedup hit and delete | **Derived** from live manifests, indexed in sled |
| Capsule record | sled only | `CAPSULE` record in the journal |
| Content index (hash → segment) | sled | **Derived**, indexed in sled |
| S3 buckets and keys | sled only | `NAMESPACE` / `BIND` / `UNBIND` records in the journal |
| Next segment id | sled counter | **Derived** at open (§8.3) |

The observation the design rests on: **ref counts are the only segment metadata that ever changes.** Once they are derived from manifests, a segment's descriptor is write-once and can sit next to its bytes. Everything else follows from that.

## 3. Files

| File | Holds | Authority |
|---|---|---|
| `space.journal` | `FILE`, `CAPSULE`, `DROP`, `NAMESPACE`, `BIND`, `UNBIND`, `BATCH`, `KEY` records | Truth for capsules, names and an encrypted store's key |
| `space.nvram` | `FILE`, `SEGMENT` records | Truth for segment bytes and descriptors |
| `<root>/store`, `<root>/segments/<id>.seg` | Under `SPACE_STORAGE_ROOT`, instead of `space.nvram`: a `FILE` record, and one `SEGMENT` record per segment file (§10) | Truth for segment bytes and descriptors |
| `space.nvram.ckpt` | Snapshot of the segment map (§8.2) | Optional cache, never authoritative |
| `space.db` | sled: capsules, refs, content index, names, `journal_applied` | Index of the journal, rebuildable |

`space.journal` lives next to `space.db`. The old `space.nvram.segments` sidecar goes away.

**Why a separate journal rather than manifests inside the data log:**

- `spacectl`, `serve-s3` included, stores segments with `AutoFsBackend` when `SPACE_STORAGE_ROOT` is set, and there is no log there. The journal works the same with every backend.
- Rebuilding the capsule index reads only the journal, which holds manifests, not gigabytes of segment data.
- Journal compaction and data compaction are independent.

The cost is one extra fsync per write: the data log before the commit, the journal for it (§6).

The journal, the data log and a storage root's `store` file each start with a `FILE` record carrying a `store_id`. Opening a journal with a data log or storage root of another store fails. A missing journal isn't replaced: the registry won't open a `space.db` that has been used without its journal, and a new journal with no records won't pair with a data log or storage root that already holds segments, since every segment would then look unreferenced ([#194](https://github.com/saworbit/SPACE/issues/194)). `spacectl` finds the journal and data log in one directory (`--store`, the current directory by default), so mixing files from two stores is an easy mistake to make. Before it opens a storage root or `space.nvram` that holds no store yet, it checks that the journal holds no records, so a wrong `SPACE_STORAGE_ROOT` fails without starting a new store there.

## 4. Record frame

Every record in every file has this frame. All integers are little-endian.

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 4 | `magic` | `SPCR` (`53 50 43 52`) |
| 4 | 1 | `kind` | §4.2 |
| 5 | 1 | `version` | Layout version of this kind. Starts at 1. |
| 6 | 2 | `flags` | Bit 0 `IGNORABLE`: a reader that doesn't know `kind` may skip the record. Other bits 0. |
| 8 | 4 | `header_len` | Bytes of header |
| 12 | 4 | `body_len` | Bytes of body |
| 16 | 16 | `body_sum` | `BLAKE3(body)[0..16]`; all zero when `body_len = 0` |
| 32 | 16 | `head_sum` | `BLAKE3(frame[0..32] ‖ header)[0..16]` |
| 48 | `header_len` | header | Layout depends on `kind` |
| 48 + `header_len` | `body_len` | body | Segment bytes; empty for every kind except `SEGMENT` |

`head_sum` covers `body_sum`, so a valid head means the lengths and body checksum can be trusted. A reader rejects `header_len > 64 MiB` or `body_len > 32 MiB` before allocating; these are sanity bounds against garbage frames, not semantic limits. A record with an unknown `kind` and no `IGNORABLE` flag stops the open: the reader fails closed rather than guess.

The sums are BLAKE3 because it's already a dependency. CRC32C would be cheaper per segment but adds a crate (open question 7).

### 4.1 Encoding primitives

| Name | Encoding |
|---|---|
| `u8` … `u64` | Little-endian |
| `uuid` | 16 bytes, RFC 4122 byte order (`Uuid::as_bytes`) |
| `hash` | 32 raw bytes |
| `tag` | 16 raw bytes |
| `str` | `u16` byte length, then UTF-8 |
| `blob` | `u32` byte length, then bytes |
| `opt_uuid` | 16 bytes; all zero means none |

### 4.2 Kinds

| Kind | Name | File | Purpose |
|---|---|---|---|
| `0x00` | `FILE` | all | First record of every file |
| `0x01` | `SEGMENT` | data log | One segment: descriptor + stored bytes |
| `0x02` | `CHECKPOINT` | checkpoint | Snapshot of the segment map |
| `0x10` | `CAPSULE` | journal | Manifest; commits a write |
| `0x11` | `DROP` | journal | Commits a delete |
| `0x12` | `NAMESPACE` | journal | Creates or deletes a namespace (an S3 bucket) |
| `0x13` | `BIND` | journal | Points a name at a capsule |
| `0x14` | `UNBIND` | journal | Removes a name |
| `0x15` | `KEY` | journal | Records an encrypted store's key (§5.9) |
| `0x1F` | `BATCH` | journal | Several journal records applied atomically |

## 5. Record layouts

### 5.1 `FILE`

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 1 | `file_kind` | 1 data log, 2 journal, 3 checkpoint, 4 storage root (§10) |
| 1 | 1 | reserved | 0 |
| 2 | 2 | `format` | Store format. 1 for this document. |
| 4 | 16 | `store_id` | `uuid`, chosen when the store is created |
| 20 | 8 | `generation` | 0 for a new file; +1 each time compaction replaces the file |
| 28 | 8 | `created_at` | Unix seconds when the file's first generation was created; compaction keeps it |
| 36 | 8 | `segment_id_floor` | The allocator never hands out an id below this (§8.3) |

### 5.2 `SEGMENT`

Header (the descriptor):

| Offset | Size | Field | From `Segment` today |
|---|---|---|---|
| 0 | 8 | `segment_id` | `id` |
| 8 | 4 | `plain_len` | `plain_len` (now required; a segment stored as is uses its stored length) |
| 12 | 2 | `flags` | Bit 0 `COMPRESSED`, bit 1 `ENCRYPTED` |
| 14 | 2 | `encryption_version` | 0 unless `ENCRYPTED` |
| 16 | 4 | `key_version` | 0 unless `ENCRYPTED` |
| 20 | 16 | `tweak` | `tweak_nonce`; zero unless `ENCRYPTED` |
| 36 | 16 | `integrity_tag` | MAC; zero unless `ENCRYPTED` |
| 52 | 32 | `content_hash` | `content_hash` as raw bytes. All zero means none: only a segment appended without a descriptor (`StorageBackend::append`, tests) has none. Every pipeline write sets it. |
| 84 | 2 + n | `compression_algo` | `str`, exactly as mixed into `content_hash` (`identity`, `lz4:1`, `zstd:3`) |

Body: the stored bytes, so `body_len` is today's `Segment.len`.

Fields of `Segment` that are not in the record, and why:

| Field | Why it's gone |
|---|---|
| `offset`, `len` | Position and `body_len` of the record |
| `ref_count`, `deduplicated` | Derived from manifests (§7) |
| `access_count` | Never updated on read; nothing uses it |
| `pq_ciphertext`, `pq_nonce` | Always `None`; there is no post-quantum code |

`body_sum` is checked on scan, so torn or rotted segment bytes are caught without a key. Reads still verify the MAC (encrypted) or `content_hash` (plaintext) as they do now.

### 5.3 `CAPSULE` (the manifest)

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 16 | `capsule_id` | `uuid` |
| 16 | 8 | `created_at` | Unix seconds |
| 24 | 8 | `size` | Logical bytes. Must equal the sum of entry `plain_len`s. |
| 32 | 32 | `digest` | BLAKE3 of the logical bytes (§11) |
| 64 | 4 | `flags` | Bit 0 `DEDUPE`: this capsule's segments go in the content index. Bit 1 `REQUIRES_ENCRYPTION`: never serve it from a segment without `ENCRYPTED`. |
| 68 | 8 | `deduped_bytes` | As today |
| 76 | 4 + n | `policy` | `blob`: the write-time `Policy` as JSON |
| 80 + n | 4 | `entry_count` | |
| 84 + n | 44 × count | entries | In logical byte order |

Entry:

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `segment_id` |
| 8 | 4 | `plain_len` |
| 12 | 32 | `content_hash` |

Notes:

- **Entries carry `content_hash`.** That binds the manifest to exact segment content: if an id ever resolves to a descriptor with a different hash, that's detected. It also lets journal replay maintain the content index without touching the data log (§7). It costs 32 bytes per 4 MiB segment.
- **Entries carry `plain_len`.** The prefix sums give each segment's logical offset, so a range read becomes a binary search over the manifest. Today `read_range_stream` fetches metadata for every segment from the start of the capsule.
- **The two flags are explicit** even though `policy` implies them. Replay and the read path never parse the policy JSON to make a security or dedup decision.
- **The policy is JSON** because it's informational, and serde defaults tolerate fields being added or removed as `Policy` is pruned. It's the only serde-encoded field in the format.
- **Size bound.** Under the 64 MiB header bound, a manifest holds about 1.5 million entries: about 6 TiB of capsule at 4 MiB segments, less with smaller segment policies.

### 5.4 `DROP`

| Offset | Size | Field |
|---|---|---|
| 0 | 16 | `capsule_id` |

### 5.5 `NAMESPACE`

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 1 | `op` | 1 create, 2 delete. Other values reserved. |
| 1 | 8 | `created_at` | Unix seconds: when it was created, or for a delete, deleted |
| 9 | 2 + n | `ns` | `str` |

A delete leaves the namespace's names alone, so a writer unbinds them in the same `BATCH`. S3 DeleteBucket does that for keys whose capsule is gone, and refuses a bucket that still holds objects ([#159](https://github.com/saworbit/SPACE/issues/159)). Deleting a namespace that doesn't exist changes nothing. A build from before op 2 refuses a journal that holds one.

### 5.6 `BIND` and `UNBIND`

`BIND` (layout version 2):

| Offset | Size | Field |
|---|---|---|
| 0 | 16 | `capsule_id` |
| 16 | 2 + n | `ns` (`str`) |
| … | 2 + m | `name` (`str`) |
| … | 2 | `count` |
| … | … | `count` pairs: `key` (`str`), then `value` (`str`) |

The pairs are the name's metadata, in the order written. The S3 view keeps the `Content-Type`, `Content-Disposition`, `Content-Encoding`, `Content-Language`, `Cache-Control`, `Expires` and `x-amz-meta-*` headers of a PUT there, with lowercase names ([#163](https://github.com/saworbit/SPACE/issues/163)). A version 1 `BIND` stops after `name`, and reads as a name with no metadata. A build from before version 2 refuses a journal that holds a version 2 `BIND`.

`UNBIND` (layout version 1) is the version 1 `BIND` without `capsule_id`.

Namespaces and names are opaque strings to the core. A namespace can't be empty or contain NUL, because the index separates a namespace from its names with NUL; a reader refuses a record that breaks this. A name can be any string. The S3 view uses `ns = "s3:" + bucket` and `name = key`. Binding a name replaces any earlier binding and its metadata; unbinding an unbound name does nothing. Neither drops a capsule on its own; that takes a `DROP` in the same `BATCH`.

### 5.7 `BATCH`

| Offset | Size | Field |
|---|---|---|
| 0 | 4 | `count` |
| 4 | … | `count` sub-records |

Sub-record:

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 1 | `kind` | `CAPSULE`, `DROP`, `NAMESPACE`, `BIND` or `UNBIND` |
| 1 | 1 | `version` | Layout version of that kind's header |
| 2 | 2 | reserved | 0 |
| 4 | 4 | `header_len` | |
| 8 | `header_len` | header | As the kind's own record |

A sub-record has no magic and no sums. The `BATCH`'s `head_sum` covers its bytes, and without the magic, the scan for a valid record after a torn `BATCH` (§8.1) can't mistake a sub-record for a record of its own.

The sub-records apply in order, all or nothing. This is what makes compound operations atomic without special-purpose fields: an S3 overwrite is one `BATCH`, so a crash can't leave the new capsule nameless or the old one alive and unreferenced.

### 5.8 `CHECKPOINT`

| Offset | Size | Field |
|---|---|---|
| 0 | 8 | `covered_offset`: the data log up to here is described |
| 8 | 4 | `count` |
| 12 | … | `count` × (`offset u64`, `header_len u32`, `SEGMENT` header bytes) |

A checkpoint file is a `FILE` record (`file_kind` 3, the data log's `store_id` and `generation`) followed by one `CHECKPOINT` record. It's written to a temp file, fsynced and renamed. It's used only if its `store_id` and `generation` match the data log.

### 5.9 `KEY`

| Offset | Size | Field | Meaning |
|---|---|---|---|
| 0 | 1 | `cipher` | 1 XTS-AES-256 with a BLAKE3 MAC. Other values reserved. |
| 1 | 3 | reserved | 0 |
| 4 | 4 | `key_version` | The key version new writes use |
| 8 | 16 | `fingerprint` | `BLAKE3-derive_key("SPACE 2026-11-13 master key fingerprint", master key)[0..16]` |

The first time a store is opened with a master key, the journal gets a `KEY` record for it ([#157](https://github.com/saworbit/SPACE/issues/157)). The last `KEY` record is the store's. Opening a store with a key whose fingerprint differs from it fails. Opening it without a key works, but writes are refused before any segment is stored, and `serve-s3` won't start, unless `SPACE_ALLOW_PLAINTEXT=1` is set. A `KEY` record is never in a `BATCH`.

The fingerprint names the key without revealing it, or any key derived from it: the derivation context is used for nothing else. Nothing changes `key_version` yet; it's there for key rotation ([#98](https://github.com/saworbit/SPACE/issues/98)).

## 6. What each operation writes

| Operation | Data log | Journal | Commit point |
|---|---|---|---|
| Write a capsule | One `SEGMENT` per new segment | `CAPSULE` | The `CAPSULE` record is durable |
| Dedup hit | Nothing | Nothing (it's an entry in the eventual `CAPSULE`) | — |
| Delete (`spacectl delete`) | Nothing | `DROP`, in a `BATCH` after an `UNBIND` for each name that points at the capsule | The record or `BATCH` |
| First open with a master key | Nothing | `KEY` | The `KEY` record is durable |
| S3 CreateBucket | Nothing | `NAMESPACE` | The `NAMESPACE` record is durable |
| S3 PUT, new key | New segments | `BATCH[CAPSULE, BIND]` | The `BATCH` |
| S3 PUT, overwrite | New segments | `BATCH[CAPSULE, BIND, DROP old]` | The `BATCH` |
| S3 DELETE | Nothing | `BATCH[UNBIND, DROP]` | The `BATCH` |
| S3 DeleteObjects | Nothing | `BATCH[UNBIND, DROP, …]` for every key that's there | The `BATCH` |
| S3 DeleteBucket | Nothing | `NAMESPACE` delete, in a `BATCH` after an `UNBIND` for each key whose capsule is gone | The record or `BATCH` |
| Multipart complete (future) | Nothing | `BATCH[CAPSULE composed, BIND, DROP part…]` | The `BATCH` |
| GC | Nothing: a segment with no references is simply not live | — | — |
| Compaction | New data log generation (§9) | — | Rename |

**Ordering rule.** The data log must be fsynced after a write's last `SEGMENT` record and before the record that commits it is appended to the journal. Then the journal is fsynced. An implementation may fsync the data log more often (today every segment commit does), but this is the only fsync correctness needs, so a write costs two.

**Pins.** An open write pins the segments it has written or dedup-hit; a streaming read pins the segments of the capsule it is reading. Pins live in memory only, and compaction treats pinned segments as live. The S3 leases from #124 become read pins: an overwrite can drop the old capsule at once, because its bytes stay readable until compaction and compaction won't take pinned segments.

**Pending dedup entries.** A segment written by an open write goes into an in-memory pending index, so a concurrent write can dedup against it. It reaches the durable content index only when its write commits. If a crashed write's hash were registered in sled, a later write could dedup-hit a segment that compaction then removed, and the capsule would be lost.

## 7. The index: applying the journal

sled holds `journal_applied = (index_version, store_id, generation, offset)`. An index whose version, store or generation doesn't match the journal is rebuilt; bump the index version whenever an index record's encoding changes. Each journal record, or each whole `BATCH`, is applied in one sled transaction that also advances `journal_applied`. Because the journal is the durable copy, sled no longer needs to flush on every update, as it does today. On open, records after `journal_applied` are replayed.

| Record | Effect on the index |
|---|---|
| `CAPSULE` | Insert the capsule. For each entry `refs[seg] += 1`. If `DEDUPE` is set and the hash isn't in the content index, register `hash → seg`. |
| `DROP` | Remove the capsule. For each entry `refs[seg] -= 1`; at 0, deregister the hash if it points at `seg`. |
| `NAMESPACE` | Create the namespace (`namespaces`), keeping the first creation time if it exists, or delete it. |
| `BIND` / `UNBIND` | Set or clear the name (`names`, keyed by namespace, NUL, name, holding the capsule id and the metadata), and file it under the capsule it points at or take it off (`capsule_names`, keyed by capsule id, then the name's key). |
| `KEY` | Nothing. The journal keeps the last one in memory, found when it's opened. |

A segment can appear more than once in one capsule; each occurrence counts. A `DROP` of a capsule that isn't there is ignored with a warning, so replay is idempotent. The writer refuses a `CAPSULE` whose id is already live; if replay meets one anyway, it reports and skips it. Within a `BATCH`, the overwrite order `CAPSULE`, `BIND`, `DROP` means segments shared by the old and new capsule never reach zero references mid-batch. Replay reads only the journal; the content hashes it registers come from manifest entries.

sled also keeps the scrub schedule (`scrub_schedule`: segment id to when it was last light- and deep-scrubbed, and any damage a scrub found that a later scrub hasn't cleared). It isn't journal state, and a rebuild keeps it, since segment ids are never reused.

**Rebuild** is: clear the sled trees and replay the journal from the start. `spacectl doctor --rebuild-index` does exactly that, and losing `space.db` costs one replay.

This fixes the ref-count drift in #97. A dedup hit writes nothing until its capsule commits, so a crash can't leave an increment behind, and a crash between commit and index update is repaired by replay.

## 8. Opening and recovery

### 8.1 Torn tails and corruption

Scanning a file, the reader validates each record's magic, `head_sum` and lengths within the file. It checks `body_sum` only for the last record in the file: a torn append can only be the last record, because each one is fsynced before the next is appended, and reading every body would mean reading the whole log on every open. Earlier bodies are verified when read (MAC or content hash) and by scrub. At the first record that fails, at offset P:

- If no valid record exists anywhere after P (the reader searches forward for the magic and checks `head_sum`), it's a torn tail from an interrupted append. The file is truncated to P and the bytes dropped are logged.
- If a valid record follows, it's corruption in the middle of the file. The store opens read-only and the damaged ranges are reported (`NvramLog::damage()` today; `spacectl doctor` should also name the capsules whose segments fall in them).

The search doesn't look inside a record that can only be the last one ([#137](https://github.com/saworbit/SPACE/issues/137)). A record whose frame and header check out, but whose bytes run past the end of the file or don't match `body_sum`, ends where its frame says, so it's a torn tail. A journal record's header can't be checked until it's whole, so when the file ends inside one, the reader walks the record's layout over the bytes there, reading only counts and string lengths. If they run out, the bytes are the start of a header, cut short: a torn tail. If a whole header fits but the frame says the record is longer, the frame is damaged, which an append cut short can't do, so it's reported as damage and nothing is truncated. The data log's `SEGMENT` header holds nothing a client chooses, so a record can't hide in one. So a name, a metadata value or a segment's bytes that hold a whole record, as a SPACE data log stored as an unencrypted capsule does, can't make a torn tail look like damage. What's left is an append whose frame didn't reach the disk while later bytes did. A record those bytes hold is then taken for one that follows, which is reported as damage, never silent data loss. `BATCH` sub-records carry no magic so that every batch doesn't trip the search (§5.7).

### 8.2 Data log

1. Read and check the `FILE` record.
2. If `space.nvram.ckpt` is valid, load the segment map from it and scan from `covered_offset`; otherwise scan from the start. A scan reads each frame and header and seeks past the body: one seek per segment, about 25,000 for 100 GB of 4 MiB segments.
3. Apply §8.1 at the end.

The checkpoint is a startup optimization only. A first implementation can skip it and always scan.

### 8.3 Journal and allocator

1. Read and check the `FILE` record; `store_id` must match the data log's.
2. Scan, applying §8.1.
3. Replay into sled after `journal_applied`.
4. Initialise the segment allocator in memory to the largest of: each file's `segment_id_floor`, 1 + the highest segment id in the data log, and 1 + the highest id in any manifest entry. The sled counter is no longer needed. Ids are never reused within a store.

After open, the orphans from interrupted writes are simply the segments in the map with `refs == 0` and no pins. No GC pass is needed to find them; compaction reclaims them.

A manifest entry whose segment isn't in the data log means the capsule is damaged (the fsync order should prevent this). It's reported, and the rest of the store opens normally.

## 9. Compaction

### 9.1 Data log ([#108](https://github.com/saworbit/SPACE/issues/108))

1. Under the pipeline lock, take the live set: segments with `refs > 0`, plus pinned segments.
2. Without the lock, write `space.nvram.compact`: a `FILE` record with `generation + 1` and `segment_id_floor` set to the allocator's next id, then the live `SEGMENT` records copied verbatim in offset order. Records hold no offsets, so their sums stay valid. Memory is one record at a time.
3. Take the log's write lock. Copy any `SEGMENT` records appended since step 2 began, fsync, close the old handle (Windows won't rename over an open file), rename over `space.nvram`, fsync the directory, reopen and release the lock.
4. Write a new checkpoint, if checkpoints are in use.

A crash before the rename leaves `space.nvram.compact` behind, which open deletes; the old log is intact. A crash after it leaves a complete new log. There's no marker file and no manual step. A segment dropped during compaction is copied anyway and reclaimed next time. §14 (PR 4) lists where the implementation differs from these steps.

### 9.2 Journal ([#136](https://github.com/saworbit/SPACE/issues/136))

Under churn the journal grows with dead records: the manifest of each deleted or overwritten capsule and the `DROP` that removed it, and each name that was replaced or removed. Compaction writes the live state to the next generation, as the data log's does:

1. Note where the journal ends.
2. Without the journal lock, read the journal to find the live capsules, namespaces and names, as replay would leave them (§7). Then write `space.journal.compact`: a `FILE` record with `generation + 1`, the last `KEY` record, a `NAMESPACE` record for each namespace with its first creation time, the `CAPSULE` record of each live capsule in journal order, and a `BIND` record for each name. Commits carry on.
3. Under the journal lock, copy the records committed since step 1 unchanged, fsync, rename the new file over `space.journal`, fsync the directory, and set `journal_applied` to the new generation and end offset.

A crash before the rename leaves `space.journal.compact`, which open deletes; the journal is intact. A crash after it leaves the new generation with a `journal_applied` from the old one, so open rebuilds the index from the new generation (§7). There's no marker file and no manual step. §14 (PR 7) has the details of the implementation.

## 10. Other backends

- **`TokioFsBackend`, `UringBackend`, `AutoFsBackend`.** Replace each segment's `segments/<id>.bin` and `metadata/<id>.json` with one file, `segments/<id>.seg`, holding a single `SEGMENT` record. Write it as `<id>.seg.tmp`, fsync, rename and fsync the directory, all before the commit record. The segment map is a directory listing; reclaiming a segment deletes its file, so there's no compaction. The `store_id` goes in a `FILE` record at `root/store`. The cold-stub path (`SPACE_STUB_V1`) is pre-reset tiering residue and goes away.
- **`InMemoryBackend`.** Gets an in-memory journal for tests.

The file backends are **done** ([#135](https://github.com/saworbit/SPACE/issues/135)), with these details:

- The layout code is shared (`storage::fs_root`); `UringBackend` uses io_uring only to read and write whole segment files.
- `root/store` holds a `FILE` record of kind 4. A root without one must be new: one holding `segments/<id>.bin`, `segments/<id>.stub.json` or a `metadata` directory is refused as predating format 1 (§13), and one holding `.seg` files is refused because it can't be paired with its journal.
- Ref counts aren't stored, as in the data log. Opening a pipeline sets them from the index, pairs the journal with the root's `store_id`, and raises the segment allocator to one past the highest segment file, so a lost `space.db` can't lead to an id that overwrites a file. Deleted ids can come back after that, which nothing depends on.
- Reads check the record's frame, `head_sum` and segment id, but not `body_sum`: the MAC or content hash checks the bytes, as for the data log. Light scrub checks both sums.
- Opening a root deletes leftover `.tmp` files, which are writes that never reached their rename. Windows can't fsync a directory, so that step is skipped there, as for the data log.
- `InMemoryBackend` has no journal of its own yet.

## 11. Logical digest

`digest` is plain, unkeyed BLAKE3 over the capsule's logical bytes. It deliberately matches `b3sum`, so a user can check a capsule against a file with standard tools. An empty capsule's digest is BLAKE3 of the empty input.

It's computed incrementally in `write_chunk` over each plaintext chunk, before compression: one BLAKE3 pass per write. It doesn't depend on policy, so it would survive re-encryption or transcoding unchanged. That makes "logical content never changes" something replay and scrub can check, not just a sentence in `capsule.md`.

Uses:
- `spacectl read` and `spacectl verify` check whole-capsule reads against it.
- Scrub can verify whole capsules, not only segments.
- The S3 view can derive a content-based ETag from it (open question 3).

A streaming read can only report a mismatch after it has sent the bytes. That's still worth having for the CLI and scrub, even though HTTP can't take bytes back.

## 12. Security notes

- **The frame sums are not MACs.** They catch torn writes and media corruption, not tampering. Tampered segment bytes are still caught by the segment MAC (encrypted) or `content_hash` (plaintext), as today.
- **Manifests aren't authenticated.** Someone who can write `space.journal` can reorder or swap a capsule's segments, just as someone who can write `space.db` can today. A keyed manifest tag is open question 1.
- **`digest` and `content_hash` are unkeyed and stored in clear, including for encrypted capsules.** Either lets someone with the files confirm a guess about content. `content_hash` already allows that in today's sidecar, so the digest adds no new kind of leak; equal ciphertext from deterministic encryption leaks equality too. A keyed digest for encrypted capsules is open question 2.
- **`store_id` isn't secret.**

## 13. Compatibility

Format 1 is new. The current data log has no `FILE` record, so open fails with a message saying the store predates format 1.

**Recommendation: no converter.** Nobody outside the project uses the current format (RESET.md). A converter is straightforward if it's ever wanted: read the sidecar and sled, then write both new files.

## 14. Code changes (sketch)

| Crate | Change |
|---|---|
| `common` | New `format` module: the record codec (pure encode/decode, no I/O). `Segment` loses `ref_count`, `deduplicated`, `access_count`, `pq_*`. `Capsule` gains `digest`. |
| `nvram-sim` | Framed `SEGMENT` appends, scan on open, tail truncation, optional checkpoint, compaction to a new generation. `increment_refcount`, `decrement_refcount`, `update_segment_metadata` and the per-append `save_segment_map` go away. |
| `storage` | `StorageTransaction::append` takes the descriptor with the bytes. `set_segment_metadata` goes away, since nothing updates a stored segment. |
| `capsule-registry` | Journal writer and replay. sled gains `refs` and `journal_applied`, loses `next_segment_id` and the per-update flush. |
| `pipeline` | Dedup hits write nothing. Pins, a pending dedup index, digest computation. `finish_write` appends the `CAPSULE` record. Delete appends `DROP`. GC becomes "nothing to do" and compaction takes over reclamation. |
| `protocol-s3` | PUT, overwrite and DELETE become `BATCH` records. Leases become pins. |
| `spacectl` | `doctor --rebuild-index`; opens `space.journal` beside `space.db`. |

Suggested PRs:

1. Codec, framed `SEGMENT` records and scan-on-open. Closes #96. **Done.** Until the journal exists:
   - Ref counts are held in memory only. Scanned segments start at 1, and opening a registry pipeline recounts them from the capsule records in sled, which already removes the ref-count inflation of #97. Compaction refuses to run until they've been recounted.
   - `Segment` keeps all its fields; `ref_count`, `deduplicated` and `access_count` just aren't written. `set_segment_metadata` on a stored segment changes only the in-memory copy.
   - Compaction is still the in-place rewrite guarded by a marker, now `space.nvram.compacting`.
   - Losing `space.db` still loses the capsule records. The data survives in the log, but nothing can say which segments make up which capsule until the journal lands.
2. Journal (`FILE`, `CAPSULE`, `DROP`, `BATCH`), derived refs, pins, digest. Fixes the ref-count half of #97; orphans become `refs == 0`. **Done**, with these differences from the draft:
   - The sled `refs` tree is maintained by journal apply, as drafted. The data log's in-memory counts, which also count open writes, are set from it when a pipeline opens; there is no separate pin structure.
   - sled also keeps an `indexed` tree, segment to content-index key, so a `DROP` that takes a segment to zero references can remove its hash.
   - The segment allocator is still the sled counter. A pipeline open raises it above the highest id in the data log, rather than deriving it in memory (§8.3).
   - A journal with damage inside it is refused rather than opened read-only.
   - Every full read of a capsule checks the digest, streamed or not ([#190](https://github.com/saworbit/SPACE/issues/190)). A range read can't, and scrub and the S3 ETag don't use it yet.
   - `BATCH` is encoded, decoded and applied, but nothing writes one until the S3 view moves onto the journal.
3. `NAMESPACE`, `BIND`, `UNBIND`; the S3 view moves onto the journal. **Done**, with these differences from the draft:
   - `BATCH` sub-records lost their frame (§5.7). Embedded whole records made a torn `BATCH` look like damage in the middle of the journal, which refuses to open.
   - Names are generic, as drafted (open question 5). sled indexes them in `namespaces` and `names`; the old `s3_buckets` and `s3_objects` trees go. A `space.db` that still holds S3 buckets or keys but has no journal position is refused, like one holding capsules. An index being rebuilt drops those trees, warning if they held anything, since the journal doesn't have them.
   - The index version is 2, so an index written before names were journaled is rebuilt.
   - Pins are the data log's in-memory reference counts. An S3 overwrite or DELETE commits its `DROP` at once, and the S3 view holds back releasing the dropped capsule's references while a GET is reading it. `Pipeline::garbage_collect` doesn't know about pins and would remove such segments, but nothing calls it while `serve-s3` runs.
   - `spacectl delete` of a named capsule keeps today's behaviour (open question 6): the name stays, and reads as missing.
   - The registry checks a commit's `CAPSULE` and `DROP` records against the index under the journal lock, so a `DROP` of a capsule that isn't live fails the whole commit and nothing is written.
4. Compaction by generation swap. Closes #108. **Done**, with these differences from §9.1:
   - Step 1 needs no pipeline lock. Open writes and readers hold segments through the data log's in-memory reference counts, so the log notes the live set itself, holding its file lock so the map matches the end offset. Appends keep the file lock until their records are in the map.
   - `segment_id_floor` is one past every id the data log has held, including ones it removed, rather than the allocator's next id; the `FILE` record is rewritten with it after the catch-up. Opening a pipeline raises the allocator to the log's floor, which is the larger of that field and one past the highest id scanned.
   - The catch-up in step 3 also copies any segment that gained references after step 1. Nothing should revive a dead segment, since nothing can find one to dedup against, but the copy costs nothing when there are none.
   - A log with nothing to reclaim isn't rewritten.
   - The old file is closed before the rename, and the new one is renamed while open. If the rename fails, the old file is reopened and the log carries on unchanged. If the reopen fails too, the log is marked lost and refuses reads and appends until the store is opened again, as the journal does (#150).
   - There's no checkpoint, so step 4 doesn't apply.
   - A `.compacting` marker from the in-place compaction of earlier builds is still refused.
5. Schedule GC and compaction, with `spacectl` commands for them (the rest of #97). **Done**:
   - The pipeline pins segments for open writes, and for each capsule a commit drops until `Pipeline::release` is called for it. `Pipeline::garbage_collect` keeps every pinned or referenced segment, so it no longer refuses to run while a write is open, and it no longer removes a segment a capsule references because its stored count is 0. This closes the gap noted under PR 3.
   - In the data log, GC adds nothing to compaction: an unreferenced segment has no references in the log's counts, so compaction leaves it out. A reclaim pass therefore compacts the data log and garbage-collects only file backends, where each segment is a file.
   - `serve-s3` runs a pass at startup and every `--reclaim-interval` seconds (default 300). It compacts once a quarter of the log, and at least one default segment, is reclaimable, without the pipeline lock. `spacectl gc` and `spacectl compact` run each step on demand.
   - `NvramLog::read` now takes the file lock before it looks up the offset. Looking it up first let a compaction swap the file in between, so the read used an offset from the old generation. The read's MAC or content-hash check would have rejected the bytes.
   - Scrub, which #97 also asks to schedule, isn't part of this.
6. Schedule scrub, with `spacectl scrub` (the last of #97). **Done**:
   - Light scrub of the data log checks each record's `head_sum` and `body_sum` (`StorageBackend::verify_stored`, `NvramLog::verify_record`), as §8.1 says scrub verifies earlier bodies. It needs no key, so it covers encrypted segments too. A length check, all light scrub did before, can't fail in the data log.
   - `serve-s3` runs `ScrubExecutor::spawn_background` with the pipeline's key, without the pipeline lock: light daily, deep weekly, at most 64 MiB/s by default. The schedule lived in memory at first, so every start began with a deep scrub of the whole store; it's now kept in `space.db` ([#134](https://github.com/saworbit/SPACE/issues/134)).
   - On a live store, a segment deleted during a cycle isn't reported as a read error, and the schedule forgets deleted segments.
   - A cycle that reaches `max_segments_per_cycle` lets the rest follow a second later. Before, they waited for the next interval.
7. Journal compaction (§9.2, [#136](https://github.com/saworbit/SPACE/issues/136)). **Done**, with these details:
   - Members of a `BATCH` come out as records of their own, each with a 48-byte frame where a sub-record has 8 bytes. A journal with little dead in it can therefore compact to more than its size, so compaction runs only when it frees bytes.
   - A name bound to a capsule that isn't live is kept, since the index keeps it (open question 6).
   - Memory: a live capsule is held as the offset of the record that holds its `CAPSULE`, which is read again when the new generation is written. Namespaces and names are held whole.
   - The `FILE` record keeps `store_id`, `created_at` and `segment_id_floor`. A journal compacted down to no records still belongs to its store, and is refused with another store's data log or storage root.
   - `serve-s3`'s reclaim pass compacts the journal once a quarter of it, and at least 1 MiB, is dead. Finding that out reads the whole journal, so a pass does it the first time after the store opens, and after that only when an estimate kept as records are appended says it may be worth it. The estimate is what the last reading found, plus every byte appended since, plus the average live `CAPSULE` record for each `DROP` among them. `spacectl compact` compacts the journal whenever anything in it is dead.
   - The index's `journal_applied` moves to the new generation under the journal lock, so no commit lands in between. sled doesn't flush on every update, so a crash shortly after a compaction can still find the old position, and the index is then rebuilt.

## 15. Tests

- Truncate the last record of each file at every byte offset. Open must succeed, and the state must equal the last commit before it.
- Flip a byte in the middle of the data log. Open must go read-only, and `doctor` must name the affected capsules.
- Proptest: random sequences of writes, dedup writes, deletes, S3 puts, overwrites and deletes. Then delete `space.db`, reopen, and compare capsules, refs, content index and names with the originals. This becomes an invariant in `capsule.md` §11: **rebuild(journal, data log) == index.** ([#139](https://github.com/saworbit/SPACE/issues/139))
- Inject a crash at each step of compaction. Reopen must succeed with no manual step.
- `digest == blake3::hash(input)` for every proptest payload.

## 16. Open questions

1. **Manifest tag.** Should encrypted stores add a keyed tag to `CAPSULE` records? It stops journal tampering, but replay then needs the key, and `spacectl list` currently works without `SPACE_MASTER_KEY`.
2. **Keyed digest.** Should encrypted capsules use a keyed digest? That closes the confirmation oracle for the digest, but `content_hash` leaks the same thing, and the digest stops matching `b3sum`.
3. **ETag.** Should it be the digest in hex, or an MD5 computed alongside it? Some S3 tools compare a single-part ETag with the body's MD5; today's UUID ETag already fails that check.
4. **Checkpoint.** Should the first implementation include the checkpoint, or always scan?
5. **Names.** Generic namespaces (as drafted), or S3-specific record kinds? *Generic, as implemented in PR 3.*
6. **Deleting a named capsule.** `spacectl delete` of a capsule an S3 key points at: keep today's behaviour (the key reads as missing), or make `DROP` also remove bindings? *Remove them, in the batch that holds the `DROP` ([#138](https://github.com/saworbit/SPACE/issues/138)). The index files each name under the capsule it points at, so a delete finds them. `DROP` itself still leaves names alone, so a key an older build left behind reads as missing, and listings skip it.*
7. **Frame sums.** BLAKE3 (no new dependency) or CRC32C (cheaper per segment)?
