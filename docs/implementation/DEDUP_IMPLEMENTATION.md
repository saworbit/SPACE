# Deduplication Implementation

SPACE deduplicates fixed-size segments by content hash. This holds across every capsule in a store, and within a single capsule. This document describes the post-reset `main` branch.

## Write path

For each segment of the input, `Pipeline::write_capsule` (in `crates/pipeline/src/lib.rs`):

1. **Splits** the input into chunks of the layout policy's segment size. The default is `LayoutStrategy::Fixed` with 4 MiB (`common::SEGMENT_SIZE`).
2. **Compresses** the chunk with `Lz4ZstdCompressor` according to `CompressionPolicy`:
   - `None`, `LZ4 { level }` or `Zstd { level }`; the default is LZ4 level 1;
   - the chunk is stored as `identity` (uncompressed) when its first 1 KiB has entropy of 7.5 bits/byte or more, or when compression doesn't make it smaller.
3. **Hashes** the stored bytes with `dedup::hash_content_with_algo(bytes, algo)`:
   ```
   BLAKE3("space.dedup.v1\0algo:" || algo || "\0" || bytes)  →  hex ContentHash
   ```
   `algo` is the compressor's label (`identity`, `lz4:1`, `zstd:3`, ...). Because the algorithm is part of the hash, the same stored bytes produced by different compression settings can never collide.
4. **Looks up** the hash in the registry's content index, a sled tree in `space.db` mapping `ContentHash → SegmentId`, then in the in-memory pending index of segments stored by writes that haven't committed yet.
   - **Hit:** increments the segment's `ref_count`, adds its ID to the capsule, and writes nothing. The capsule's `deduped_bytes` grows by the segment's compressed size.
   - **Miss:** allocates a new `SegmentId` (a counter in sled), encrypts if the policy says to, appends the segment's record to the log, and puts the hash in the pending index.
5. **Commits** the capsule: appends its `CAPSULE` manifest to `space.journal`, with each segment's plain length and content hash. Applying it to the index adds the capsule record, raises each segment's reference count, and moves its hashes into the durable content index (unless the policy turned dedup off). A write that never commits leaves nothing in the durable index.

Dedup happens before encryption, and the hash is taken over the pre-encryption bytes. [ENCRYPTED_DEDUP_FLOW.md](ENCRYPTED_DEDUP_FLOW.md) explains how the two fit together.

## Known gaps

- The hash domain includes the encryption mode, so an encrypted write never reuses a plaintext segment. It doesn't include the key version, so data written after a key rotation can reuse segments encrypted under the old key ([#98](https://github.com/saworbit/SPACE/issues/98)).
- Chunks are fixed-size. Inserting bytes near the start of a file shifts every later chunk boundary, so an edited file shares little with the original. There is no content-defined chunking.
- `deduped_bytes` counts compressed bytes.

## Reference counting, deletion and GC

- Each segment's `ref_count` is held in memory and never written. Opening a pipeline sets it from the per-segment reference counts the index keeps, which only committed capsules contribute to, so a crash can't leave a count too high or too low.
- `Pipeline::delete_capsule` decrements the count for each of the capsule's segments. When a count reaches zero it drops the segment from the log's in-memory map and unregisters the hash. The segment's record stays in the log until `NvramLog::compact()` writes the next generation without it; reopening the log in the meantime maps it again, with no references.
- `Pipeline::garbage_collect` removes segments that no capsule references and nothing pins, such as leftovers from failed or interrupted writes. An open write pins its segments, and a capsule an S3 overwrite or DELETE dropped pins its own until it's released.
- `spacectl delete` calls `delete_capsule`. S3 DELETE and overwrites commit the `DROP` in the same `BATCH` as the key change, then release the dropped capsule's segments the same way once no GET is reading it.
- `serve-s3` runs a reclaim pass at startup and every 5 minutes: it compacts `space.nvram` once a quarter of it, and at least 4 MiB, is reclaimable, or garbage-collects a `SPACE_STORAGE_ROOT` store. `spacectl gc` and `spacectl compact` run the two steps on demand.

## Using it

From the CLI:

```bash
spacectl create -f big.bin
spacectl create -f big.bin    # second copy: space.nvram doesn't grow
spacectl list                 # two capsules referencing the same segments
```

From Rust:

```rust
use capsule_registry::{pipeline::WritePipeline, CapsuleRegistry};
use nvram_sim::NvramLog;

let registry = CapsuleRegistry::open("space.db")?;
let pipeline = WritePipeline::new(registry.clone(), NvramLog::open("space.nvram")?);
let a = pipeline.write_capsule(&data).await?;
let b = pipeline.write_capsule(&data).await?;   // reuses a's segments
let (segment_refs, unique_segments) = registry.get_dedup_stats();
```

`get_dedup_stats` returns the total number of segment references across all capsules and the number of entries in the content index.

## Measurements

These numbers come from a test pass on a Ryzen 9 5900X running Windows 11:

- Creating the same 10 MiB random file twice through `spacectl create` added 0 bytes to `space.nvram` the second time.
- For hashing throughput on its own, run `cargo bench -p capsule-registry --bench pipeline -- dedup_hash_blake3`.

## Scaling limits

- Each segment's metadata is written once, in its record in `space.nvram`: a 48-byte frame and a descriptor of about 90 bytes. Dedup hits and deletes write nothing to the log.
- Opening the log reads every record's header, and rebuilding the index replays the whole journal. Both grow with the store; a normal open replays only the records the index hasn't applied.
- Content lookups are point reads in sled.

## Tests

```bash
cargo test -p dedup                                         # hashing, verify_content_hash
cargo test -p capsule-registry --test proptest_pipeline     # dedup, distinct payloads, cross-policy separation
cargo test -p capsule-registry --test integration_test
cargo test -p capsule-registry --test gc_test               # ref counts and orphan sweep
```

## Troubleshooting

**Identical files aren't deduplicating**
- Check that both writes used the same compression settings: the algorithm is part of the hash.
- Check that the files are byte-identical from the start. Chunks are fixed-size, so an offset shift changes every chunk.
- Each segment's compression decision is logged at `INFO` (the default level).

**Disk use doesn't drop after deleting**
- Deleting a capsule only drops its segments' references. Compaction reclaims the bytes: `serve-s3` runs it once a quarter of the log is reclaimable, and `spacectl compact` runs it at once.
