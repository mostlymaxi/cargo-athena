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
ARGO_VERSION="${ARGO_VERSION:-v4.0.8}"  # CI overrides per matrix entry
# ATHENA_E2E_SINGLE=1 → 1-node cluster (hosts without kind cross-node
# networking, e.g. NixOS default-drop FORWARD). Default is the 3-node split.
KIND_CFG="$SCRIPT_DIR/kind-cluster.yaml"
if [ "${ATHENA_E2E_SINGLE:-0}" = "1" ]; then
  KIND_CFG="$SCRIPT_DIR/kind-cluster-single.yaml"
fi
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
  echo "config: $KIND_CFG"
  kind create cluster --config "$KIND_CFG"
fi
kubectl config use-context "kind-$CLUSTER"
kubectl wait --for=condition=Ready nodes --all --timeout=120s

say "Argo Workflows $ARGO_VERSION"
kubectl create namespace argo --dry-run=client -o yaml | kubectl apply -f -
# v4 CRDs exceed kubectl's last-applied annotation limit -> server-side.
kubectl apply --server-side --force-conflicts -n argo -f \
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

say "workflow executor RBAC (default SA)"
# Argo's emissary reports step outputs via workflowtaskresults; the
# workflow pods run as the namespace 'default' SA, which namespace-install
# does not grant. Without this every step Errors with a 403.
#
# Apply a literal Role/RoleBinding instead of `kubectl create role
# --resource=workflowtaskresults.argoproj.io`: that form does client-side
# API discovery and, if the Argo CRD isn't established yet (a race on
# fresh installs), prints nothing -> "no objects passed to apply" -> exit
# 1. RBAC rules are plain strings, not validated against discovery, so a
# manifest applies deterministically regardless of CRD readiness.
kubectl apply -f - <<'EOF'
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: athena-executor
  namespace: argo
rules:
- apiGroups: ["argoproj.io"]
  resources: ["workflowtaskresults"]
  verbs: ["create", "patch"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: athena-executor-default
  namespace: argo
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: athena-executor
subjects:
- kind: ServiceAccount
  name: default
  namespace: argo
EOF

say "controller RBAC for large-args ConfigMap offload (Argo 3.7+)"
# Argo's container-args offload (workflow/controller/workflowpod.go L497+
# in 4.0.5, gated by PR #15265 since 3.7) creates a ConfigMap holding
# the original `c.Args` JSON when their JSON-marshaled size exceeds
# 128 KB. The upstream `namespace-install.yaml` grants the controller SA
# only `get/list/watch` on configmaps; without `create` here, any task
# whose substituted args cross the threshold errors immediately with
#   "configmaps is forbidden: ... cannot create resource configmaps".
# 3.6 lacks the offload code entirely (the binding is harmless there).
kubectl apply -f - <<'EOF'
apiVersion: rbac.authorization.k8s.io/v1
kind: Role
metadata:
  name: athena-argo-configmaps
  namespace: argo
rules:
- apiGroups: [""]
  resources: ["configmaps"]
  verbs: ["create", "update", "patch", "delete"]
---
apiVersion: rbac.authorization.k8s.io/v1
kind: RoleBinding
metadata:
  name: athena-argo-configmaps
  namespace: argo
roleRef:
  apiGroup: rbac.authorization.k8s.io
  kind: Role
  name: athena-argo-configmaps
subjects:
- kind: ServiceAccount
  name: argo
  namespace: argo
EOF

say "point Argo at MinIO"
kubectl -n argo patch configmap workflow-controller-configmap \
  --type merge --patch-file "$SCRIPT_DIR/artifact-repo-cm.yaml"
kubectl -n argo rollout restart deploy/workflow-controller

say "wait for readiness"
kubectl -n argo rollout status deploy/workflow-controller --timeout=240s
kubectl -n argo rollout status deploy/minio --timeout=240s
kubectl -n argo wait --for=condition=complete job/minio-mkbucket --timeout=180s

say "ready — run: nix develop -c scripts/e2e-test.sh"
