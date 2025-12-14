#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
MOCKUP_DIR="${ROOT_DIR}/UI_mockup"
PORT="${PORT:-4173}"

if ! command -v npm >/dev/null 2>&1; then
  echo "npm is required to view the UI mockup." >&2
  exit 1
fi

if [ ! -d "${MOCKUP_DIR}" ]; then
  echo "UI_mockup directory not found at ${MOCKUP_DIR}." >&2
  exit 1
fi

if [ ! -d "${MOCKUP_DIR}/node_modules" ]; then
  echo "Installing UI mockup dependencies..."
  npm install --prefix "${MOCKUP_DIR}"
fi

echo "Starting Vite dev server on http://localhost:${PORT} (Ctrl+C to stop)..."
npm run --prefix "${MOCKUP_DIR}" dev -- --host --port "${PORT}"
