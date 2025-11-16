# ─────────────────────────────────────────────────────────────────────
# SPACE CORE – Production image < 180 MB, zero-trust, health-checked
# Build:  docker build -f containerization/Dockerfile -t space-core:latest .
# ─────────────────────────────────────────────────────────────────────
FROM rust:1.78-bookworm AS builder
WORKDIR /build
# Cache deps forever
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
RUN cargo fetch --locked

# Build ONLY production crates
RUN cargo build --release --locked \
    -p spacectl -p protocol-s3 -p capsule-registry

# ───── Runtime ─────
FROM ubuntu:24.04
RUN apt-get update && apt-get install -y ca-certificates curl && rm -rf /var/lib/apt/lists/*

# Copy exact binaries
COPY --from=builder \
  /build/target/release/spacectl \
  /build/target/release/protocol-s3 \
  /build/target/release/capsule-registry \
  /usr/local/bin/

# Non-root + locked dirs
RUN useradd -u 1000 -m space && \
    mkdir -p /capsules /logs && \
    chown space:space /capsules /logs

USER space
WORKDIR /capsules

# Smart launcher
COPY containerization/entrypoint.sh /usr/local/bin/
ENTRYPOINT ["/usr/local/bin/entrypoint.sh"]
CMD ["spacectl", "--help"]

# Healthcheck for S3 API
HEALTHCHECK --interval=15s --timeout=3s --start-period=5s \
  CMD curl -f http://localhost:8080/health || exit 1

LABEL org.opencontainers.image.title="SPACE Core" \
      space.version="3.0" \
      space.purpose="Adaptive Encrypted Dedup S3"
