#!/usr/bin/env bash
set -euxo pipefail

CLUSTER=space-phase4-chaos
kind create cluster --name "$CLUSTER" --config deployment/kind-config.yaml
trap "kind delete cluster --name $CLUSTER" EXIT

kubectl create namespace chaos-testing || true
helm repo add chaos-mesh https://charts.chaos-mesh.org
helm repo update
helm install chaos-mesh chaos-mesh/chaos-mesh --namespace=chaos-testing --create-namespace

cat <<EOF | kubectl apply -f -
apiVersion: chaos-mesh.org/v1alpha1
kind: NetworkChaos
metadata:
  name: partition-node
  namespace: chaos-testing
spec:
  action: partition
  mode: one
  selector:
    namespaces:
      - default
  direction: both
  duration: '30s'
EOF

UUID=550e8400-e29b-41d4-a716-446655440000
POLICY=examples/phase4-policy.yaml
./target/release/spacectl project --view csi --id "$UUID" --policy-file "$POLICY"

kubectl logs -n chaos-testing -l app.kubernetes.io/name=chaos-mesh || true
helm uninstall chaos-mesh --namespace=chaos-testing
kubectl delete namespace chaos-testing || true
