# Building SPACE

Requires Rust 1.89 or newer (the committed `Cargo.lock` needs it).

```bash
cargo test --workspace
cargo build --release -p spacectl
cargo run -p spacectl -- --help
```

`spacectl serve-s3` listens on port 8080. Set `SPACE_STORAGE_ROOT` to store segments on a filesystem path instead of the NVRAM log.
