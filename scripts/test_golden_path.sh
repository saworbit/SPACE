#!/usr/bin/env bash
set -euo pipefail

# ==============================================================================
# SPACE: Golden Path Verification
# Lifecycle: Build -> S3 View -> Tier -> Mesh -> UI -> WASM Transforms
# ==============================================================================

RED='\033[0;31m'
GREEN='\033[0;32m'
BLUE='\033[0;34m'
NC='\033[0m'

log() { echo -e "${BLUE}[SPACE]${NC} $1"; }
pass() { echo -e "${GREEN}[PASS]${NC} $1"; }
fail() { echo -e "${RED}[FAIL]${NC} $1"; exit 1; }

ROOT="$(pwd)"
TMP="$(mktemp -d)"

UI_PORT="${UI_PORT:-}"
UI_GOSSIP_PORT="${UI_GOSSIP_PORT:-}"
S3_PORT="${S3_PORT:-}"

SPACECTL="${SPACECTL:-./target/release/spacectl.exe}"
WEBSERVER_BIN="${WEBSERVER_BIN:-./target/release/web-server.exe}"

cleanup() {
  local exit_code="$?"
  set +e
  for pid in "${S3_PID:-}" "${WEB_PID:-}" "${P1:-}" "${P2:-}" "${P3:-}"; do
    [[ -n "${pid}" ]] || continue
    kill "${pid}" 2>/dev/null || true
    wait "${pid}" 2>/dev/null || true
  done
  if [[ "${exit_code}" == "0" ]]; then
    rm -rf "${TMP}"
  else
    echo -e "${RED}[FAIL]${NC} Logs preserved in ${TMP}" >&2
  fi
  cd "${ROOT}" >/dev/null 2>&1 || true
}
trap cleanup EXIT

require_cmd() {
  command -v "$1" >/dev/null 2>&1 || fail "Missing required command: $1"
}

require_cmd cargo
require_cmd curl

pick_free_port() {
  if command -v powershell.exe >/dev/null 2>&1; then
    powershell.exe -NoProfile -Command '$l=[System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback,0);$l.Start();$p=$l.LocalEndpoint.Port;$l.Stop();$p' \
      | tr -d '\r'
    return
  fi
  if command -v python >/dev/null 2>&1; then
    python - <<'PY'
import socket
s = socket.socket()
s.bind(("127.0.0.1", 0))
print(s.getsockname()[1])
s.close()
PY
    return
  fi
  fail "Unable to select a free port (need powershell.exe or python)"
}

if [[ -z "${UI_PORT}" ]]; then
  UI_PORT="$(pick_free_port)"
fi
if [[ -z "${UI_GOSSIP_PORT}" ]]; then
  UI_GOSSIP_PORT="$(pick_free_port)"
fi
if [[ "${UI_GOSSIP_PORT}" == "${UI_PORT}" ]]; then
  UI_GOSSIP_PORT="$(pick_free_port)"
fi
if [[ -z "${S3_PORT}" ]]; then
  S3_PORT="$(pick_free_port)"
fi
if [[ "${S3_PORT}" == "${UI_PORT}" ]]; then
  S3_PORT="$(pick_free_port)"
fi
log "Selected ports: ui=${UI_PORT} ui_gossip=${UI_GOSSIP_PORT} s3=${S3_PORT}"

# 1) Clean slate
if [[ "${SPACE_SKIP_CLEAN:-}" != "1" && "${SPACE_SKIP_CLEAN:-}" != "true" ]]; then
  log "Cleaning previous state..."
  ./scripts/clean.sh || true
else
  log "Skipping clean (SPACE_SKIP_CLEAN=1)"
fi

# 2) Build world
log "Building spacectl (phase5) + web-server..."
cargo build --release -p spacectl --features phase5
cargo build --release -p web-interface --bin web-server

[[ -x "${SPACECTL}" ]] || fail "spacectl not found/executable at ${SPACECTL}"
[[ -x "${WEBSERVER_BIN}" ]] || fail "web-server not found/executable at ${WEBSERVER_BIN}"

if [[ "${SPACECTL}" != /* && ! "${SPACECTL}" =~ ^[A-Za-z]: ]]; then
  SPACECTL="$(cd "$(dirname "${SPACECTL}")" && pwd)/$(basename "${SPACECTL}")"
fi
if [[ "${WEBSERVER_BIN}" != /* && ! "${WEBSERVER_BIN}" =~ ^[A-Za-z]: ]]; then
  WEBSERVER_BIN="$(cd "$(dirname "${WEBSERVER_BIN}")" && pwd)/$(basename "${WEBSERVER_BIN}")"
fi

# 3) Start UI (Web Interface)
log "Starting Web Interface on :${UI_PORT}..."
BIND_ADDR="127.0.0.1:${UI_PORT}" \
GOSSIP_ADDR="127.0.0.1:${UI_GOSSIP_PORT}" \
"${WEBSERVER_BIN}" >"${TMP}/web.log" 2>&1 &
WEB_PID=$!

for _ in $(seq 1 50); do
  if curl -fsS "http://127.0.0.1:${UI_PORT}/health" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done
if ! curl -fsS "http://127.0.0.1:${UI_PORT}/health" >/dev/null 2>&1; then
  tail -n 80 "${TMP}/web.log" 2>/dev/null || true
  fail "Web Interface health check failed (see ${TMP}/web.log)"
fi

HTTP_CODE="$(curl -o /dev/null -s -w "%{http_code}\n" "http://127.0.0.1:${UI_PORT}/")"
if [[ "${HTTP_CODE}" == "200" ]]; then
  pass "Web Interface is online."
else
  fail "Web Interface unreachable (HTTP ${HTTP_CODE}). See ${TMP}/web.log"
fi

# 4) Start S3 view (modular pipeline + tiering)
log "Starting S3 server on :${S3_PORT} (modular + tiering)..."
SPACE_STORAGE_ROOT_POSIX="${TMP}/space.storage"
SPACE_COLD_ROOT_POSIX="${TMP}/space.cold"
mkdir -p "${SPACE_STORAGE_ROOT_POSIX}" "${SPACE_COLD_ROOT_POSIX}"
if command -v cygpath >/dev/null 2>&1; then
  export SPACE_STORAGE_ROOT="$(cygpath -w "${SPACE_STORAGE_ROOT_POSIX}")"
  export SPACE_COLD_ROOT="$(cygpath -w "${SPACE_COLD_ROOT_POSIX}")"
else
  export SPACE_STORAGE_ROOT="${SPACE_STORAGE_ROOT_POSIX}"
  export SPACE_COLD_ROOT="${SPACE_COLD_ROOT_POSIX}"
fi
export SPACE_COLD_THRESHOLD_SECS="${SPACE_COLD_THRESHOLD_SECS:-0}"
export SPACE_TIER_SCAN_INTERVAL_SECS="${SPACE_TIER_SCAN_INTERVAL_SECS:-1}"
export SPACE_TIER_MAX_SEGMENTS_PER_SCAN="${SPACE_TIER_MAX_SEGMENTS_PER_SCAN:-256}"

S3_WORKDIR="${TMP}/s3.workdir"
mkdir -p "${S3_WORKDIR}"
(
  cd "${S3_WORKDIR}"
  exec "${SPACECTL}" serve-s3 --port "${S3_PORT}" --modular
) >"${TMP}/s3.log" 2>&1 &
S3_PID=$!

for _ in $(seq 1 100); do
  if curl -fsS "http://127.0.0.1:${S3_PORT}/demo-bucket" >/dev/null 2>&1; then
    break
  fi
  sleep 0.1
done

# 5) Write + read via S3 (View layer)
log "Writing object via S3 view..."
PAYLOAD="Hello Space Universe"
RESP_HEADERS="${TMP}/put.headers"
curl -sS -D "${RESP_HEADERS}" -o /dev/null -X PUT \
  "http://127.0.0.1:${S3_PORT}/demo-bucket/hello.txt" \
  --data-binary "${PAYLOAD}"

ETAG="$(tr -d '\r' <"${RESP_HEADERS}" | awk -F': ' 'tolower($1)=="etag"{print $2}' | tr -d '"')"
[[ -n "${ETAG}" ]] || fail "S3 PUT did not return an ETag (capsule id). See ${TMP}/s3.log"
log "Capsule ID (ETag): ${ETAG}"

RECOVERED="$(curl -fsS "http://127.0.0.1:${S3_PORT}/demo-bucket/hello.txt")"
if [[ "${RECOVERED}" == "${PAYLOAD}" ]]; then
  pass "S3 GET returned original bytes."
else
  fail "S3 GET mismatch. Got: '${RECOVERED}'"
fi

# 6) Tiering check (Metal)
log "Waiting for tiering stub (SPACE_STUB_V1)..."
HOT_SEG_DIR="${SPACE_STORAGE_ROOT_POSIX}/segments"
SEG_FILE=""
for _ in $(seq 1 100); do
  if [[ -d "${HOT_SEG_DIR}" ]]; then
    SEG_FILE="$(ls -1t "${HOT_SEG_DIR}"/*.bin 2>/dev/null | head -n 1 || true)"
  fi
  [[ -n "${SEG_FILE}" ]] && break
  sleep 0.1
done
[[ -n "${SEG_FILE}" ]] || fail "No hot segment file found under ${HOT_SEG_DIR}"

for _ in $(seq 1 100); do
  if grep -q "SPACE_STUB_V1" "${SEG_FILE}" 2>/dev/null; then
    break
  fi
  sleep 0.1
done

if grep -q "SPACE_STUB_V1" "${SEG_FILE}" 2>/dev/null; then
  pass "Tiering stub detected: ${SEG_FILE}"
else
  fail "Tiering stub not detected in time (file: ${SEG_FILE}). See ${TMP}/s3.log"
fi

RECOVERED_AFTER_TIER="$(curl -fsS "http://127.0.0.1:${S3_PORT}/demo-bucket/hello.txt")"
if [[ "${RECOVERED_AFTER_TIER}" == "${PAYLOAD}" ]]; then
  pass "S3 GET succeeded after tiering (rehydration path OK)."
else
  fail "S3 GET after tiering mismatch. Got: '${RECOVERED_AFTER_TIER}'"
fi

# 7) Bootstrap Mesh (Raft + gossip)
log "Bootstrapping Mesh (3 nodes)..."

N1_GOSSIP="127.0.0.1:$(pick_free_port)"
N2_GOSSIP="127.0.0.1:$(pick_free_port)"
N3_GOSSIP="127.0.0.1:$(pick_free_port)"

N1_RAFT="127.0.0.1:$(pick_free_port)"
N2_RAFT="127.0.0.1:$(pick_free_port)"
N3_RAFT="127.0.0.1:$(pick_free_port)"

log "Mesh ports: n1_gossip=${N1_GOSSIP} n1_raft=${N1_RAFT}"
log "Mesh ports: n2_gossip=${N2_GOSSIP} n2_raft=${N2_RAFT}"
log "Mesh ports: n3_gossip=${N3_GOSSIP} n3_raft=${N3_RAFT}"

"${SPACECTL}" server start \
  --node-id 1 \
  --bootstrap \
  --gossip-addr "${N1_GOSSIP}" \
  --raft-addr "${N1_RAFT}" \
  --metadata-path "${TMP}/node1.meta.db" \
  --raft-store-path "${TMP}/node1.raft.db" \
  --gossip-seed "${N2_GOSSIP}" \
  --gossip-seed "${N3_GOSSIP}" \
  >"${TMP}/node1.log" 2>&1 &
P1=$!

"${SPACECTL}" server start \
  --node-id 2 \
  --join "${N1_RAFT}" \
  --gossip-addr "${N2_GOSSIP}" \
  --raft-addr "${N2_RAFT}" \
  --metadata-path "${TMP}/node2.meta.db" \
  --raft-store-path "${TMP}/node2.raft.db" \
  --gossip-seed "${N1_GOSSIP}" \
  >"${TMP}/node2.log" 2>&1 &
P2=$!

"${SPACECTL}" server start \
  --node-id 3 \
  --join "${N1_RAFT}" \
  --gossip-addr "${N3_GOSSIP}" \
  --raft-addr "${N3_RAFT}" \
  --metadata-path "${TMP}/node3.meta.db" \
  --raft-store-path "${TMP}/node3.raft.db" \
  --gossip-seed "${N1_GOSSIP}" \
  >"${TMP}/node3.log" 2>&1 &
P3=$!

for _ in $(seq 1 100); do
  if "${SPACECTL}" server status --addr "${N1_RAFT}" 2>/dev/null | grep -Eq 'voters:.*1.*2.*3'; then
    break
  fi
  sleep 0.2
done

"${SPACECTL}" server status --addr "${N1_RAFT}" | grep -Eq 'voters:.*1.*2.*3' \
  || fail "Cluster did not converge (see ${TMP}/node*.log)"
pass "Mesh cluster membership converged."

# 8) Verify replication via Raft (metadata)
log "Replicating capsule metadata via Raft..."
"${SPACECTL}" registry put --addr "${N1_RAFT}" --id "${ETAG}" --size "${#PAYLOAD}" --segment 1 >/dev/null

for _ in $(seq 1 100); do
  if "${SPACECTL}" registry get --addr "${N3_RAFT}" --id "${ETAG}" | grep -q "\"id\""; then
    break
  fi
  sleep 0.2
done

"${SPACECTL}" registry get --addr "${N3_RAFT}" --id "${ETAG}" | grep -q "\"id\"" \
  && pass "Metadata visible on Node 3 (Raft replication OK)." \
  || fail "Metadata not visible on Node 3."

# 9) Leader failover
log "Forcing leader failover (kill node 1)..."
kill "${P1}" 2>/dev/null || true
wait "${P1}" 2>/dev/null || true

for _ in $(seq 1 100); do
  if "${SPACECTL}" server status --addr "${N2_RAFT}" 2>/dev/null | grep -Eq 'leader_id: (2|3)'; then
    break
  fi
  sleep 0.2
done

"${SPACECTL}" server status --addr "${N2_RAFT}" | grep -Eq 'leader_id: (2|3)' \
  || fail "No new leader elected after node 1 death."
pass "Leader election completed."

# 10) Phase 5 sanity: WASM transforms test suite
log "Running Phase 5 WASM transform smoke tests..."
cargo test -p pipeline --features phase5 --test wasm_transforms_test >/dev/null
pass "WASM transform engine verified."

echo -e "${GREEN}========================================${NC}"
echo -e "${GREEN}       MISSION ACCOMPLISHED             ${NC}"
echo -e "${GREEN}========================================${NC}"
