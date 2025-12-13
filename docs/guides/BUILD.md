# Building SPACE with S3 Protocol View

## Quick Build

```bash
# Clean build (recommended for first time)
cargo clean
cargo build --release

# This will take 2-5 minutes on first build
```

## What Gets Built

```
target/release/
├── spacectl           # CLI tool + S3 server
└── libprotocol_s3.so  # S3 protocol library
```

## Run Tests

### Unit Tests
```bash
# Test individual crates
cargo test -p common
cargo test -p capsule-registry
cargo test -p nvram-sim
cargo test -p protocol-s3
```

### Integration Tests
```bash
# Test the full pipeline
cargo test --workspace

# Run with output
cargo test --workspace -- --nocapture

# Metro-sync / DataMotion copy + move paths
cargo test -p scaling --test data_motion_test
```

### S3 View Tests
```bash
# Specific S3 protocol tests
cargo test -p protocol-s3 -- --nocapture

# You should see:
# ✅ PUT: Created capsule
# ✅ GET: Retrieved data
# ✅ HEAD: Verified metadata
# ✅ LIST: Found objects
# ✅ DELETE: Object removed
```

## Development Build

For faster iteration during development:

```bash
# Debug build (much faster, but slower runtime)
cargo build

# Run debug binary
./target/debug/spacectl serve-s3
```

## Common Issues

### Issue: `error: could not compile protocol-s3`

**Solution:** Make sure all dependencies are in workspace Cargo.toml:
```bash
grep -A3 "\[workspace.dependencies\]" Cargo.toml
```

### Issue: `cannot find crate protocol-s3`

**Solution:** Verify workspace members:
```bash
# Should include protocol-s3
cargo metadata --no-deps --format-version 1 | grep protocol-s3
```

### Issue: Tests fail with "file not found"

**Solution:** Tests create temporary files. Clean up:
```bash
rm -f test*.nvram* test*.metadata space.nvram* space.db
cargo test --workspace
```

### Issue: Port already in use

**Solution:** Kill existing server or use different port:
```bash
# Find process
lsof -i :8080

# Kill it
kill -9 <PID>

# Or use different port
./target/release/spacectl serve-s3 --port 9000
```

## Verification Checklist

After building, verify everything works:

```bash
# 1. Check binary exists
ls -lh target/release/spacectl

# 2. Check help works
./target/release/spacectl --help

# 3. Run unit tests
cargo test --workspace

# 4. Start server
./target/release/spacectl serve-s3 &
SERVER_PID=$!

# 5. Test health endpoint
curl http://localhost:8080/health

# 6. Run demo
chmod +x scripts/demo_s3.sh
./scripts/demo_s3.sh

# 7. Stop server
kill $SERVER_PID
```

## Clean Build

If you encounter weird errors, try:

```bash
# Nuclear option - rebuild everything
cargo clean
rm -f space.nvram* space.db test*.nvram* test*.metadata
cargo build --release
```

## Quality Gates

Run these before opening a PR:
```bash
cargo fmt --all
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo audit
```

### Linux RDMA Zero-Copy (Phase C)

```bash
# Enable RDMA transport on Linux hosts (requires libibverbs)
cargo build -p scaling --features "rdma"

# Optional: bring up SoftRoCE for CI/dev without hardware
sudo scripts/setup_softroce.sh eth0

# If running clippy/check in containers, ensure LibTorch is available for layout-engine
# (Dockerfile installs CPU libtorch by default). To bypass the check in ad-hoc runs:
# XTASK_ALLOW_MISSING_LIBTORCH=1 cargo xtask audit

# RDMA implementation details and hardening steps:
# See docs/specs/PHASE_C_IMPLEMENTATION_DETAILS.md and docs/specs/EXECUTION_PLAN_PHASE_C_RDMA_SYSTEM_HARDENING.md for handshake/QP state machine specifics.
```

## Build Optimization

### Release Build Flags (already in workspace Cargo.toml)

```toml
[profile.release]
opt-level = 3        # Maximum optimization
lto = true           # Link-time optimization
codegen-units = 1    # Better optimization, slower build
strip = true         # Remove debug symbols
```

### Faster Incremental Builds

```bash
# Use mold linker (Linux) for faster linking
cargo install mold

# Set in .cargo/config.toml:
# [target.x86_64-unknown-linux-gnu]
# linker = "clang"
# rustflags = ["-C", "link-arg=-fuse-ld=mold"]
```

## Next Steps

Once built successfully:

1. ✅ Read [QUICKSTART_S3.md](QUICKSTART_S3.md) for usage examples
2. ✅ Run `./scripts/demo_s3.sh` to see it in action
3. ✅ Start building the next protocol view (NFS or Block)

---

**Build time estimates:**
- First build (release): 3-5 minutes
- Incremental rebuild: 10-30 seconds
- Clean rebuild: 2-4 minutes
