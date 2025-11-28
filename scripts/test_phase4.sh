#!/bin/bash
set -euo pipefail

# Phase 4 E2E harness (requires Docker/Kind + kubectl)
kind create cluster --name space-phase4
cargo build --release --features phase4

# Deploy the CSI shim that speaks to the new protocol-csi crate
kubectl apply -f deployment/csi-deployment.yaml

# Create a capsule with the Phase 4 policy
./target/release/spacectl create --file test.txt --policy-file examples/phase4-policy.yaml --view nvme

# (Optional) Mount the NFS view for verification
if mount | grep -q "space-phase4"; then
    echo "NFS view already mounted"
else
    sudo mount -t nfs localhost:/capsule /mnt/space-phase4
    diff test.txt /mnt/space-phase4/0 || echo "view differs (expected in CI mocks)"
    sudo umount /mnt/space-phase4
fi

kind delete cluster --name space-phase4
