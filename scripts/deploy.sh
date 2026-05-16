#!/usr/bin/env bash
# Stand up the kind e2e environment:
#   - 3-node kind cluster (1 control-plane, 2 workers)
#   - Argo Workflows (controller+server) pinned to the control node
#   - MinIO (artifact repository) on the control node + bucket
#   - athena-s3 secret + controller artifactRepository -> MinIO
#
# Run inside the dev shell:  nix develop -c scripts/deploy.sh
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CLUSTER=athena-e2e
ARGO_VERSION=v3.6.10
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

for t in kind kubectl; do
  command -v "$t" >/dev/null || { echo "missing '$t' — run via: nix develop -c $0"; exit 1; }
done

# kind needs a container provider.
if docker info >/dev/null 2>&1; then
  :
elif command -v podman >/dev/null && podman info >/dev/null 2>&1; then
  export KIND_EXPERIMENTAL_PROVIDER=podman
  echo "using podman provider"
else
  echo "need a running Docker or Podman daemon for kind"; exit 1
fi

say "kind cluster"
if kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  echo "cluster '$CLUSTER' already exists"
else
  kind create cluster --config "$SCRIPT_DIR/kind-cluster.yaml"
fi
kubectl config use-context "kind-$CLUSTER"
kubectl wait --for=condition=Ready nodes --all --timeout=120s

say "Argo Workflows $ARGO_VERSION"
kubectl create namespace argo --dry-run=client -o yaml | kubectl apply -f -
kubectl apply -n argo -f \
  "https://github.com/argoproj/argo-workflows/releases/download/${ARGO_VERSION}/namespace-install.yaml"

# Pin the controller + server onto the control node.
PIN='{"spec":{"template":{"spec":{"nodeSelector":{"athena.dev/role":"control"},"tolerations":[{"key":"node-role.kubernetes.io/control-plane","operator":"Exists","effect":"NoSchedule"}]}}}}'
for d in workflow-controller argo-server; do
  kubectl -n argo patch deploy "$d" --type strategic -p "$PIN"
done

say "MinIO + bucket"
kubectl apply -f "$SCRIPT_DIR/minio.yaml"

say "athena-s3 secret"
kubectl -n argo create secret generic athena-s3 \
  --from-literal=accessKey=athena \
  --from-literal=secretKey=athena12345 \
  --dry-run=client -o yaml | kubectl apply -f -

say "point Argo at MinIO"
kubectl -n argo patch configmap workflow-controller-configmap \
  --type merge --patch-file "$SCRIPT_DIR/artifact-repo-cm.yaml"
kubectl -n argo rollout restart deploy/workflow-controller

say "wait for readiness"
kubectl -n argo rollout status deploy/workflow-controller --timeout=240s
kubectl -n argo rollout status deploy/minio --timeout=240s
kubectl -n argo wait --for=condition=complete job/minio-mkbucket --timeout=180s

say "ready — run: nix develop -c scripts/e2e-test.sh"
