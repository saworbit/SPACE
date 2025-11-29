#!/usr/bin/env bash
set -euo pipefail

if [[ "$(uname -s)" != "Linux" ]]; then
  echo "This probe is intended for Linux hosts so the io_uring transport is active." >&2
  exit 1
fi

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
cd "${REPO_ROOT}"

export RUST_LOG="${RUST_LOG:-info,scaling=debug}"
export FRAME_COUNT="${FRAME_COUNT:-512}"
export FRAME_BYTES="${FRAME_BYTES:-262144}" # 256KiB per frame to stress the queue

echo "Building scaling crate (io_uring path is compiled on Linux)..."
cargo build -p scaling --release

echo "Running io_uring replication probe example..."
FRAME_COUNT="${FRAME_COUNT}" FRAME_BYTES="${FRAME_BYTES}" \
  RUST_LOG="${RUST_LOG}" \
  cargo run -p scaling --release --example uring_probe

echo
echo "Expected signals of zero-copy + backpressure:"
echo "  - Logs: 'io_uring enqueue replication frame' / 'io_uring send queue above 80%'"
echo "  - No warning about TCP fallback (indicates Linux-only path is active)"
echo "Optional deeper check (requires strace + root):"
echo "  sudo strace -f -eio_uring_setup,io_uring_enter cargo run -p scaling --release --example uring_probe"
