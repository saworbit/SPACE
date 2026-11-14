# Build the single-node SPACE CLI.
#   docker build -t space-core:latest .
#   docker run --rm space-core:latest --help
#
# Stores live in /data, the working directory. Mount a volume there to keep
# them, and bind beyond the container's loopback to reach serve-s3:
#   docker run --rm -v space-data:/data -p 8080:8080 space-core:latest serve-s3 --bind 0.0.0.0
#
# Base images are pinned by digest; Dependabot updates the digests.

# Match the workspace rust-version (CI checks this), and the runtime image's
# Debian release.
FROM rust:1.89-bookworm@sha256:948f9b08a66e7fe01b03a98ef1c7568292e07ec2e4fe90d88c07bb14563c84ff AS builder
WORKDIR /usr/src/space
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
RUN cargo build --release --locked -p spacectl

FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
# spacectl makes no TLS connections, so the image needs no CA certificates.
RUN useradd --system --uid 10001 --user-group --home-dir /data --create-home --shell /usr/sbin/nologin space
COPY --from=builder /usr/src/space/target/release/spacectl /usr/local/bin/spacectl
USER 10001:10001
WORKDIR /data
EXPOSE 8080
ENTRYPOINT ["spacectl"]
CMD ["--help"]
