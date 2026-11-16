# Build the single-node SPACE CLI.
#   docker build -t space-core:latest .
#   docker run --rm space-core:latest --help
#
# Stores live in /data, the working directory. Mount a volume there to keep
# them, and bind beyond the container's loopback to reach serve-s3:
#   docker run --rm -v space-data:/data -p 8080:8080 space-core:latest serve-s3 --bind 0.0.0.0
# docker stop lets requests in flight finish for --shutdown-grace seconds
# (default 5) before serve-s3 exits.
#
# The image runs as uid 10001. To bind-mount a host directory instead of a
# volume, make it writable by that uid or run with --user "$(id -u):$(id -g)".
#
# Base images are pinned by digest, and Dependabot updates the digests. The
# builder tracks current stable Rust; CI checks the workspace still builds on
# its rust-version. The Linux release binaries are built in the same builder
# image and checked on the same runtime image: release-build.yml reads both
# FROM lines, so keep their form.
FROM rust:1-bookworm@sha256:ade39a710ef85f3506cffcfec3e8a24ba090badef1dff2fb798f71d878833fea AS builder
WORKDIR /usr/src/space
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
RUN cargo build --release --locked -p spacectl

# The same Debian release as the builder, so the binary finds the glibc it
# was linked against.
FROM debian:bookworm-slim@sha256:734a72b3d6142a71993b4bc580ca01d8ca687f77a2c422d96a961d3376b179bc
# spacectl makes no TLS connections, so the image needs no CA certificates.
RUN useradd --system --uid 10001 --user-group --home-dir /data --create-home --shell /usr/sbin/nologin space
COPY --from=builder /usr/src/space/target/release/spacectl /usr/local/bin/spacectl
USER 10001:10001
WORKDIR /data
EXPOSE 8080
ENTRYPOINT ["spacectl"]
CMD ["--help"]
