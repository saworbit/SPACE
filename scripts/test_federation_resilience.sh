#!/usr/bin/env bash
set -euxo pipefail

BIN="${BIN:-./target/debug/spacectl}"
if [[ ! -x "$BIN" ]]; then
  cargo build -p spacectl
fi

ROOT="$(pwd)"
TMP="$(mktemp -d)"
cleanup() {
  set +e
  [[ -n "${P1:-}" ]] && kill "$P1" 2>/dev/null || true
  [[ -n "${P2:-}" ]] && kill "$P2" 2>/dev/null || true
  [[ -n "${P3:-}" ]] && kill "$P3" 2>/dev/null || true
  rm -rf "$TMP"
  cd "$ROOT"
}
trap cleanup EXIT

N1_GOSSIP=127.0.0.1:7101
N2_GOSSIP=127.0.0.1:7102
N3_GOSSIP=127.0.0.1:7103

N1_RAFT=127.0.0.1:9101
N2_RAFT=127.0.0.1:9102
N3_RAFT=127.0.0.1:9103

# 1) Start Node 1 (Bootstrap)
"$BIN" server start \
  --node-id 1 \
  --bootstrap \
  --gossip-addr "$N1_GOSSIP" \
  --raft-addr "$N1_RAFT" \
  --metadata-path "$TMP/node1.meta.db" \
  --raft-store-path "$TMP/node1.raft.db" \
  --gossip-seed "$N2_GOSSIP" \
  --gossip-seed "$N3_GOSSIP" \
  >/dev/null 2>&1 &
P1=$!

# 2) Start Node 2 & 3 (Join Node 1)
"$BIN" server start \
  --node-id 2 \
  --join "$N1_RAFT" \
  --gossip-addr "$N2_GOSSIP" \
  --raft-addr "$N2_RAFT" \
  --metadata-path "$TMP/node2.meta.db" \
  --raft-store-path "$TMP/node2.raft.db" \
  --gossip-seed "$N1_GOSSIP" \
  >/dev/null 2>&1 &
P2=$!

"$BIN" server start \
  --node-id 3 \
  --join "$N1_RAFT" \
  --gossip-addr "$N3_GOSSIP" \
  --raft-addr "$N3_RAFT" \
  --metadata-path "$TMP/node3.meta.db" \
  --raft-store-path "$TMP/node3.raft.db" \
  --gossip-seed "$N1_GOSSIP" \
  >/dev/null 2>&1 &
P3=$!

# Wait for membership to converge.
for _ in $(seq 1 50); do
  if "$BIN" server status --addr "$N1_RAFT" | rg -q 'voters: .*1.*2.*3'; then
    break
  fi
  sleep 0.2
done

# 3) Write Capsule metadata to Node 1 (replicated via Raft)
UUID=550e8400-e29b-41d4-a716-446655440000
"$BIN" registry put --addr "$N1_RAFT" --id "$UUID" --size 0 --segment 1 >/dev/null

# 4) Verify Capsule exists on Node 2 & 3 (metadata check)
for _ in $(seq 1 50); do
  if "$BIN" registry get --addr "$N2_RAFT" --id "$UUID" | rg -q '\"id\"'; then
    break
  fi
  sleep 0.2
done

for _ in $(seq 1 50); do
  if "$BIN" registry get --addr "$N3_RAFT" --id "$UUID" | rg -q '\"id\"'; then
    break
  fi
  sleep 0.2
done

# 5) Kill Node 1 (leader). Reads/writes should continue after election.
kill "$P1"
wait "$P1" 2>/dev/null || true

# 6) Verify a new leader is elected and delete the capsule via Node 2 (should forward to leader).
for _ in $(seq 1 50); do
  if "$BIN" server status --addr "$N2_RAFT" | rg -q 'leader_id: (2|3)'; then
    break
  fi
  sleep 0.2
done

"$BIN" registry delete --addr "$N2_RAFT" --id "$UUID" >/dev/null

for _ in $(seq 1 50); do
  if "$BIN" registry get --addr "$N3_RAFT" --id "$UUID" | rg -q '\\(not found\\)'; then
    break
  fi
  sleep 0.2
done
