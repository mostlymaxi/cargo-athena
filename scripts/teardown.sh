#!/usr/bin/env bash
# Tear down the kind e2e environment.
#   Run inside the dev shell:  nix develop -c scripts/teardown.sh
set -euo pipefail

CLUSTER=athena-e2e

# Drop any lingering MinIO port-forward from e2e-test.sh.
pkill -f "kubectl.*port-forward.*svc/minio" 2>/dev/null || true

if command -v podman >/dev/null && ! docker info >/dev/null 2>&1; then
  export KIND_EXPERIMENTAL_PROVIDER=podman
fi

if kind get clusters 2>/dev/null | grep -qx "$CLUSTER"; then
  kind delete cluster --name "$CLUSTER"
  echo "deleted kind cluster '$CLUSTER'"
else
  echo "no kind cluster '$CLUSTER'"
fi

rm -f /tmp/athena-wf.yaml /tmp/athena-wts.yaml /tmp/athena-doc-*.yaml 2>/dev/null || true
