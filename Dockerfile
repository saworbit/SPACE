# Core SPACE Dockerfile
#
# Builds production-ready images for SPACE components (spacectl, io-engine, metadata-mesh)
# WITHOUT simulation crates to minimize image size and attack surface.
#
# Build: docker build -t space-core:latest .
# Run: docker run space-core:latest spacectl --help

# ============================================================================
# Builder Stage: Compile Rust workspace (excluding sim-* crates)
# ============================================================================
FROM rust:1.78 as builder

WORKDIR /usr/src/space

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
COPY vendor/ ./vendor/
COPY xtask/ ./xtask/

# Build release binaries, excluding simulation crates
# Note: --workspace builds all members, so we exclude sim-* explicitly
RUN cargo build --release \
    --workspace \
    --exclude sim-nvram \
    --exclude sim-nvmeof \
    --exclude sim-other \
    --exclude xtask

# ============================================================================
# Runtime Stage: Minimal Ubuntu with only necessary binaries
# ============================================================================
FROM ubuntu:24.04

# Install runtime dependencies
RUN apt-get update && apt-get install -y \
    ca-certificates \
    libssl3 \
    && rm -rf /var/lib/apt/lists/*

# Copy compiled binaries from builder
COPY --from=builder /usr/src/space/target/release/spacectl /usr/local/bin/
# Note: io-engine and metadata-mesh are embedded in spacectl or protocol crates
# Adjust if separate binaries exist:
# COPY --from=builder /usr/src/space/target/release/io-engine /usr/local/bin/
# COPY --from=builder /usr/src/space/target/release/metadata-mesh /usr/local/bin/

# Create non-root user for security
RUN useradd -m -u 1000 space && \
    mkdir -p /data /var/log/space && \
    chown -R space:space /data /var/log/space

USER space
WORKDIR /data

# Default command: Run spacectl
# Override with docker run args for different components
CMD ["spacectl", "--help"]

# Health check for S3 server (if running protocol-s3)
# HEALTHCHECK --interval=30s --timeout=3s \
#   CMD curl -f http://localhost:8080/health || exit 1

# Metadata
LABEL org.opencontainers.image.title="SPACE Core" \
      org.opencontainers.image.description="Policy-Driven Object Data Management System" \
      org.opencontainers.image.vendor="Adaptive Storage" \
      org.opencontainers.image.licenses="Apache-2.0"
