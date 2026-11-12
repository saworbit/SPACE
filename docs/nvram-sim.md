# NVRAM Log (`nvram-sim`)

`nvram_sim::NvramLog` is the append-only segment log that `storage::NvramBackend` wraps. It's the default backend for `spacectl` and `serve-s3`.

The record layout (`space.nvram`: a `FILE` record, then one framed `SEGMENT` record per segment) is in [on-disk-format.md](on-disk-format.md). For opening, crash behaviour and compaction, see [capsule.md §4.2](capsule.md#42-nvram-log-layout) and [§12](capsule.md#12-failure-modes-and-recovery).
