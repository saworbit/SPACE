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
4. **Looks up** the hash in the registry's content index, a sled tree in `space.db` mapping `ContentHash → SegmentId`.
   - **Hit:** increments the segment's `ref_count`, adds its ID to the capsule, and writes nothing. The capsule's `deduped_bytes` grows by the segment's compressed size.
   - **Miss:** allocates a new `SegmentId` (a counter in sled), encrypts if the policy says to, appends the bytes to the log, writes the segment metadata with `ref_count: 1`, and registers the hash.
5. **Creates** the capsule record with its ordered list of segment IDs.

Dedup happens before encryption, and the hash is taken over the pre-encryption bytes. [ENCRYPTED_DEDUP_FLOW.md](ENCRYPTED_DEDUP_FLOW.md) explains how the two fit together.

## Known gaps

- The hash domain includes the encryption mode, so an encrypted write never reuses a plaintext segment. It doesn't include the key version, so data written after a key rotation can reuse segments encrypted under the old key ([#98](https://github.com/saworbit/SPACE/issues/98)).
- Chunks are fixed-size. Inserting bytes near the start of a file shifts every later chunk boundary, so an edited file shares little with the original. There is no content-defined chunking.
- `deduped_bytes` counts compressed bytes.

## Reference counting, deletion and GC

- Each segment's `ref_count` lives in its metadata record in `space.nvram.segments`.
- `Pipeline::delete_capsule` decrements the count for each of the capsule's segments. When a count reaches zero it removes the segment record and unregisters the hash. The bytes stay in the log until `NvramLog::compact()` rewrites it.
- `Pipeline::garbage_collect` removes segments that no capsule references, such as leftovers from failed or interrupted writes.
- `spacectl delete`, S3 DELETE and S3 overwrites call `delete_capsule`. Nothing runs GC or compaction ([#97](https://github.com/saworbit/SPACE/issues/97)).

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
- Each segment costs about 490 bytes in `space.nvram.segments`, which is pretty-printed JSON.
- For hashing throughput on its own, run `cargo bench -p capsule-registry --bench pipeline -- dedup_hash_blake3`.

## Scaling limits

- The whole `space.nvram.segments` file is rewritten on every append and every ref-count change, which costs about 1.4 MB per write at 3,000 segments ([#96](https://github.com/saworbit/SPACE/issues/96)).
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
- Deleting a capsule only removes its segment records. Compaction reclaims the bytes, and nothing schedules it yet ([#97](https://github.com/saworbit/SPACE/issues/97)).
