#!/bin/bash
set -euo pipefail

# Failover script that forces federation to find a new home when a node disappears.
kind create cluster --name space-failover
cargo build --release --features phase4

# Launch two mesh agents for redundancy (mocked via pods or local processes)
# In a real deployment you would run `mesh-node` binaries; here we assume the KIND nodes already have pods.
kubectl apply -f deployment/csi-deployment.yaml

# Create a capsule with a high-latency target so federation is exercised.
./target/release/spacectl create --file test.txt --policy-file examples/phase4-policy.yaml --view nvme

# Simulate a mesh node failure by deleting one of the pods.
kubectl get pods -l app=space-csi -o name | head -n 1 | xargs -r kubectl delete

# Retry the view; the mesh should resolve a new target before giving the capsule back.
./target/release/spacectl read --view nvme $(./target/release/spacectl list | head -n 1)

kind delete cluster --name space-failover
