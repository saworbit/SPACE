#!/usr/bin/env bash
set -euo pipefail

# Run a full NVMe/TCP round-trip using the sim target:
# 1) Start sim-nvmeof
# 2) nvme discover + connect
# 3) Write/read one 4KiB block and verify contents

if [[ "${EUID:-$(id -u)}" -ne 0 ]]; then
    echo "This script must run as root so nvme-cli can create devices." >&2
    exit 1
fi

ADDR="${LISTEN_ADDR:-127.0.0.1}"
PORT="${LISTEN_PORT:-4420}"
NQN="${SUBSYSTEM_NQN:-nqn.2024-01.io.space:sim}"
BACKING_PATH="${BACKING_PATH:-/tmp/sim_nvmeof.img}"
NODE_ID="${NODE_ID:-sim-node1}"
KEEP_ALIVE="${NVME_SIM_KEEP_ALIVE:-0}"
NVME_BIN="${NVME_BIN:-nvme}"
CARGO_BIN="${CARGO_BIN:-cargo}"
NVME_DEV="${NVME_DEV:-}"

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
    if [[ -n "${NVME_DEV:-}" ]]; then
        "$NVME_BIN" disconnect -n "$NQN" >/dev/null 2>&1 || true
    fi
    rm -f /tmp/nvmeof_write.bin /tmp/nvmeof_read.bin
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

discover_and_connect() {
    "$NVME_BIN" discover -t tcp -a "$ADDR" -s "$PORT" >/tmp/nvmeof_discover.log
    "$NVME_BIN" connect -t tcp -n "$NQN" -a "$ADDR" -s "$PORT"
}

find_device() {
    if [[ -n "$NVME_DEV" ]]; then
        return
    fi

    local dev
    dev=$("$NVME_BIN" list -o json 2>/dev/null | python - "$NQN" <<'PY'
import json, sys
nqn = sys.argv[1]
data = json.load(sys.stdin)
for dev in data.get("Devices", []):
    if dev.get("SubsystemNQN") == nqn and "DevicePath" in dev:
        print(dev["DevicePath"])
        sys.exit(0)
sys.exit(1)
PY
)
    if [[ -z "$dev" ]]; then
        dev="/dev/nvme0n1"
    fi
    NVME_DEV="$dev"
}

write_and_verify() {
    dd if=/dev/urandom of=/tmp/nvmeof_write.bin bs=4096 count=1 status=none
    "$NVME_BIN" write "$NVME_DEV" --data=/tmp/nvmeof_write.bin --data-size=4096 --lba=0 --namespace-id=1
    "$NVME_BIN" read "$NVME_DEV" --data=/tmp/nvmeof_read.bin --data-size=4096 --lba=0 --namespace-id=1
    cmp /tmp/nvmeof_write.bin /tmp/nvmeof_read.bin
}

echo "[nvmeof] starting sim-nvmeof on ${ADDR}:${PORT} (NQN=${NQN})"
start_sim
wait_for_port

echo "[nvmeof] running nvme discover + connect..."
discover_and_connect
find_device
echo "[nvmeof] using device ${NVME_DEV}"

echo "[nvmeof] issuing write/read round-trip..."
write_and_verify

echo "[nvmeof] I/O validation succeeded"
