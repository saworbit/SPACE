# Build the single-node SPACE CLI.
# docker build -t space-core:latest .
# docker run --rm space-core:latest --help

FROM rust:1.83 AS builder
WORKDIR /usr/src/space
COPY Cargo.toml Cargo.lock ./
COPY crates/ ./crates/
RUN cargo build --release -p spacectl

FROM debian:bookworm-slim
RUN apt-get update && apt-get install -y ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=builder /usr/src/space/target/release/spacectl /usr/local/bin/spacectl
USER nobody
WORKDIR /data
ENTRYPOINT ["spacectl"]
CMD ["--help"]
