#!/bin/bash
set -euo pipefail

# Phase D validation: project NVMe view and exercise discovery mocks.

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
TARGET="${ROOT_DIR}/target/release/spacectl"

if [ ! -x "${TARGET}" ]; then
  echo "spacectl binary not found at ${TARGET}. Build with 'cargo build --release --features phase4' first."
  exit 1
fi

echo "[phase4] starting SPACE node"
"${TARGET}" daemon &
NODE_PID=$!
trap 'kill ${NODE_PID} ${PROJECT_PID:-} >/dev/null 2>&1 || true' EXIT

sleep 1

echo "[phase4] writing capsule data"
CAP_ID=$("${TARGET}" write --data "Phase4 Data")

echo "[phase4] projecting NVMe view for capsule ${CAP_ID}"
"${TARGET}" project --view nvme --id "${CAP_ID}" --policy-file "${ROOT_DIR}/examples/phase4-policy.yaml" &
PROJECT_PID=$!

sleep 1

echo "[phase4] verifying NVMe discovery output"
if "${ROOT_DIR}/scripts/nvmeof_discover.sh" | grep -q "${CAP_ID}"; then
  echo "[phase4] NVMe view discovered successfully"
else
  echo "[phase4] failed to discover NVMe view"
  exit 1
fi

kill "${PROJECT_PID}" "${NODE_PID}"
trap - EXIT
echo "[phase4] test completed"
