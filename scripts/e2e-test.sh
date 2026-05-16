#!/usr/bin/env bash
# Full e2e against the kind cluster from scripts/deploy.sh:
#   cross-compile -> upload binary tarball to MinIO -> emit templates ->
#   apply WorkflowTemplates -> submit Workflow -> assert it Succeeded,
#   ran on the worker nodes, and produced the save_artifact! object.
#
# Run inside the dev shell:  nix develop -c scripts/e2e-test.sh
set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
CLUSTER=athena-e2e
NS=argo
PKG=cargo-athena-example-integration
BIN=integration
export ATHENA_CONFIG="$ROOT/scripts/athena.toml"
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

for t in cargo kubectl argo mc jq; do
  command -v "$t" >/dev/null || { echo "missing '$t' — run via: nix develop -c $0"; exit 1; }
done
kubectl config use-context "kind-$CLUSTER" >/dev/null

PF=""
cleanup() { [ -n "$PF" ] && kill "$PF" 2>/dev/null || true; }
trap cleanup EXIT

say "cross-compile + package (cargo athena build)"
( cd "$ROOT" && cargo run -q -p cargo-athena-cli -- athena build \
    --package "$PKG" --bin "$BIN" )
TARBALL="$ROOT/target/athena/${BIN}.tar.gz"
test -f "$TARBALL" || { echo "missing $TARBALL"; exit 1; }

say "upload binary tarball to MinIO"
kubectl -n "$NS" port-forward svc/minio 9000:9000 >/dev/null 2>&1 &
PF=$!
until (exec 3<>/dev/tcp/127.0.0.1/9000) 2>/dev/null; do sleep 1; done
mc alias set athena-e2e http://127.0.0.1:9000 athena athena12345 >/dev/null
mc cp "$TARBALL" \
  athena-e2e/athena-artifacts/athena/bin/integration/0.1.0/integration.tar.gz

say "emit WorkflowTemplates + Workflow"
( cd "$ROOT" && cargo run -q -p cargo-athena-cli -- athena emit \
    --package "$PKG" --bin "$BIN" ) > /tmp/athena-wf.yaml

rm -f /tmp/athena-doc-*.yaml
awk 'BEGIN{n=1} /^---$/{n++; next} {print >> ("/tmp/athena-doc-" n ".yaml")}' \
  /tmp/athena-wf.yaml
: > /tmp/athena-wts.yaml
WFDOC=""
for f in /tmp/athena-doc-*.yaml; do
  if grep -qx 'kind: Workflow' "$f"; then
    WFDOC="$f"
  else
    cat "$f" >> /tmp/athena-wts.yaml
    echo '---' >> /tmp/athena-wts.yaml
  fi
done
test -n "$WFDOC" || { echo "no Workflow doc emitted"; exit 1; }

say "apply WorkflowTemplates"
kubectl apply -n "$NS" -f /tmp/athena-wts.yaml

say "submit Workflow (waits for completion)"
argo submit -n "$NS" --wait --log "$WFDOC"

say "assertions"
PHASE=$(argo get -n "$NS" @latest -o json | jq -r '.status.phase')
echo "phase: $PHASE"
[ "$PHASE" = "Succeeded" ] || { echo "FAIL: workflow phase $PHASE"; exit 1; }

NODES=$(kubectl -n "$NS" get pods -l workflows.argoproj.io/workflow \
  --no-headers -o custom-columns=N:.spec.nodeName | sort -u)
echo "pod nodes:"; echo "$NODES" | sed 's/^/  /'
if [ "${ATHENA_E2E_SINGLE:-0}" = "1" ]; then
  echo "single-node mode: skipping worker/control placement assertion"
else
  if echo "$NODES" | grep -qx "${CLUSTER}-control-plane"; then
    echo "FAIL: a workflow pod ran on the control node"; exit 1
  fi
  echo "$NODES" | grep -q "${CLUSTER}-worker" || {
    echo "FAIL: no workflow pod ran on a worker"; exit 1; }
fi

if mc ls --recursive athena-e2e/athena-artifacts/outputs/ 2>/dev/null \
   | grep -q 'result-note'; then
  echo "save_artifact! object found in MinIO"
else
  echo "FAIL: save_artifact! object not found in MinIO"; exit 1
fi

say "PASS — full e2e green"
