#!/usr/bin/env bash
set -euo pipefail

# Smoke test: start the sim-nvmeof target and run `nvme discover` against it.

ADDR="${LISTEN_ADDR:-127.0.0.1}"
PORT="${LISTEN_PORT:-4420}"
NQN="${SUBSYSTEM_NQN:-nqn.2024-01.io.space:sim}"
BACKING_PATH="${BACKING_PATH:-/tmp/sim_nvmeof.img}"
NODE_ID="${NODE_ID:-sim-node1}"
KEEP_ALIVE="${NVME_SIM_KEEP_ALIVE:-0}"
NVME_BIN="${NVME_BIN:-nvme}"
CARGO_BIN="${CARGO_BIN:-cargo}"

if ! command -v "$NVME_BIN" >/dev/null 2>&1; then
    echo "nvme CLI not found (set NVME_BIN if using a non-default path)" >&2
    exit 1
fi

if ! command -v "$CARGO_BIN" >/dev/null 2>&1; then
    echo "cargo not found (set CARGO_BIN if installed elsewhere)" >&2
    exit 1
fi

SIM_PID=""
cleanup() {
    if [[ -n "$SIM_PID" && "$KEEP_ALIVE" != "1" ]]; then
        kill "$SIM_PID" 2>/dev/null || true
        wait "$SIM_PID" 2>/dev/null || true
    fi
}
trap cleanup EXIT

start_sim() {
    if [[ -n "$SIM_PID" ]]; then
        return
    fi

    RUST_LOG="${RUST_LOG:-info}" \
    NODE_ID="$NODE_ID" \
    BACKING_PATH="$BACKING_PATH" \
    LISTEN_ADDR="$ADDR" \
    LISTEN_PORT="$PORT" \
    SUBSYSTEM_NQN="$NQN" \
        "$CARGO_BIN" run -p sim-nvmeof --bin sim-nvmeof --quiet --release &

    SIM_PID=$!
}

wait_for_port() {
    python - <<'PY'
import socket, os, sys, time
addr = os.environ["ADDR"]
port = int(os.environ["PORT"])
deadline = time.time() + 30
while time.time() < deadline:
    with socket.socket(socket.AF_INET, socket.SOCK_STREAM) as s:
        s.settimeout(1.0)
        if s.connect_ex((addr, port)) == 0:
            sys.exit(0)
    time.sleep(0.5)
print(f"Timed out waiting for {addr}:{port}", file=sys.stderr)
sys.exit(1)
PY
}

echo "[nvmeof] starting sim-nvmeof on ${ADDR}:${PORT} (NQN=${NQN})"
start_sim
wait_for_port

echo "[nvmeof] running nvme discover..."
"$NVME_BIN" discover -t tcp -a "$ADDR" -s "$PORT"

echo "[nvmeof] discover completed successfully"
