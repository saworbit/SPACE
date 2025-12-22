# Autonomous Tiering (Phase 6: The Metal)

SPACE can offload cold segment payloads from hot local storage to a cheap S3-compatible object store and leave behind a small JSON stub. Reads remain transparent: if a segment is stubbed, it is automatically rehydrated.

## Concepts

- **Hot**: local `segments/<id>.bin` payloads.
- **Cold**: S3-compatible object store objects (currently backed by a local directory via `object_store::local::LocalFileSystem`).
- **Stub**: JSON written into `segments/<id>.bin` with `magic: "SPACE_STUB_V1"` + remote pointer + checksum.
- **Thermostat**: in-memory Heatmap tracking segment access frequency; TieringAgent periodically migrates cold candidates.

## Enable tiering (filesystem backend)

Tiering is currently wired through the modular pipeline + filesystem storage backend.

1. Build and run the S3 server in modular mode.

```bash
cargo build -p spacectl --release --features modular_pipeline
```

```bash
export SPACE_STORAGE_ROOT=/path/to/space-storage
export SPACE_COLD_ROOT=/path/to/space-cold-objects
./target/release/spacectl serve-s3 --port 8080 --modular
```

2. Optional knobs:

- `SPACE_COLD_BUCKET` (default: `bucket`) used for formatting `s3://...` stub URLs.
- `SPACE_COLD_THRESHOLD_SECS` (default: 30 days) age threshold for “cold”.
- `SPACE_TIER_SCAN_INTERVAL_SECS` (default: 60) scan cadence.
- `SPACE_TIER_MAX_SEGMENTS_PER_SCAN` (default: 256) scan batch size.
- `SPACE_REHEAT_ON_READ` (default: false) if true, rehydrated bytes are written back to hot storage (stub removed).

## Stub format

`segments/<id>.bin` contains JSON:

- `magic`: `"SPACE_STUB_V1"`
- `original_size`: original payload size
- `remote_url`: e.g. `s3://bucket/segments/<id>.bin`
- `checksum`: e.g. `sha256:<hex>`

