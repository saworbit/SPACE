#!/usr/bin/env bash
set -euxo pipefail

BIN="${BIN:-./target/debug/spacectl}"
if [[ ! -x "$BIN" ]]; then
  cargo build -p spacectl --features phase4
fi

ROOT="$(pwd)"
TMP="$(mktemp -d)"

cleanup() {
  set +e
  [[ -n "${P2:-}" ]] && kill "$P2" 2>/dev/null || true
  rm -rf "$TMP"
  cd "$ROOT"
}
trap cleanup EXIT

SECRET="test-secret"
ZONE_A_DIR="$TMP/zone-a"
ZONE_B_DIR="$TMP/zone-b"
SPACE_HOME_DIR="$TMP/home"

mkdir -p "$ZONE_A_DIR" "$ZONE_B_DIR" "$SPACE_HOME_DIR"

# Start Node B (Zone 2) federation receiver.
"$BIN" federation serve \
  --addr 127.0.0.1:9001 \
  --metadata-path "$ZONE_B_DIR/space.db" \
  --nvram-path "$ZONE_B_DIR/space.nvram" \
  --secret "$SECRET" \
  >/dev/null 2>&1 &
P2=$!

# Configure Node A to know about Node B.
SPACE_HOME="$SPACE_HOME_DIR" "$BIN" zone add \
  --name zone-2 \
  --url http://127.0.0.1:9001 \
  --secret "$SECRET" \
  >/dev/null

POLICY_FILE="$TMP/policy.yaml"
cat >"$POLICY_FILE" <<'YAML'
federation:
  - zone-2
YAML

DATA_FILE="$TMP/data.bin"
printf 'hello federation\n' >"$DATA_FILE"

# Write capsule in Zone A with federation policy targeting Zone B.
OUT="$(cd "$ZONE_A_DIR" && SPACE_HOME="$SPACE_HOME_DIR" "$BIN" put "$DATA_FILE" --policy-file "$POLICY_FILE")"
CAPSULE_ID="$(printf '%s\n' "$OUT" | head -n 1)"

# Allow background replication to complete.
sleep 5

# Stop the receiver and verify the capsule exists in Zone B's local store.
kill "$P2"
wait "$P2" 2>/dev/null || true
P2=""

(cd "$ZONE_B_DIR" && "$BIN" read "$CAPSULE_ID" >"$TMP/out.bin")
cmp "$DATA_FILE" "$TMP/out.bin"

echo "ok: replicated capsule $CAPSULE_ID into zone-2"

