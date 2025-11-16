#!/usr/bin/env bash
set -euo pipefail
for mod in $(echo "$SIM_MODULES" | tr ',' ' '); do
  echo "Starting sim-$mod"
  nvram-sim --module "$mod" --dir /sim &
done
wait
