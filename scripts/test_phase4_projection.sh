#!/usr/bin/env bash
set -euo pipefail

# Phase 4 "Project" test: zero-install, legacy-compatible view projection.
#
# Note: this repository's FUSE view is simulated; the mount creates a `content`
# file (FIFO on Unix) that streams capsule bytes from the pipeline on demand.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$ROOT_DIR"

SPACECTL="${SPACECTL:-$ROOT_DIR/target/debug/spacectl}"

if [[ ! -x "$SPACECTL" ]]; then
  echo "Building spacectl (phase4)…" >&2
  cargo build -p spacectl --features phase4 >/dev/null
fi

UUID="${UUID:-$(python - <<'PY'
import uuid
print(uuid.uuid4())
PY
)}"

TMPDIR="${TMPDIR:-/tmp}"
HELLO_TXT="$TMPDIR/hello.txt"
MOUNT_DIR="$TMPDIR/space-view"

echo "Hello World" > "$HELLO_TXT"

echo "Creating capsule $UUID…" >&2
"$SPACECTL" put "$HELLO_TXT" --id "$UUID"

mkdir -p "$MOUNT_DIR"

echo "Mounting capsule view…" >&2
"$SPACECTL" project mount --id "$UUID" --target "$MOUNT_DIR" &
PID=$!

cleanup() {
  kill "$PID" >/dev/null 2>&1 || true
}
trap cleanup EXIT

sleep 1

echo "Reading via standard tools…" >&2
cat "$MOUNT_DIR/content"
