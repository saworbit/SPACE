#!/usr/bin/env bash
set -euxo pipefail

CLUSTER=space-phase4
kind create cluster --name "$CLUSTER" --config deployment/kind-config.yaml
trap "kind delete cluster --name $CLUSTER" EXIT

cargo build --release --features phase4
kubectl apply -f deployment/csi-driver.yaml

UUID=550e8400-e29b-41d4-a716-446655440000
POLICY=examples/phase4-policy.yaml

./target/release/spacectl project --view nvme --id "$UUID" --policy-file "$POLICY"
./target/release/spacectl project --view nfs --id "$UUID" --policy-file "$POLICY"
./target/release/spacectl project --view fuse --id "$UUID" --policy-file "$POLICY"
./target/release/spacectl project --view csi --id "$UUID" --policy-file "$POLICY"

# quick sanity checks
kubectl get pods -n kube-system
nvme discover -t tcp -a localhost -s 4420 || true

