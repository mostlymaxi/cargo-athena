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
PKG=cargo-athena-example-e2e
BIN=e2e
export ATHENA_CONFIG="$ROOT/scripts/athena.toml"
say() { printf '\n\033[1;36m== %s\033[0m\n' "$*"; }

for t in cargo kubectl argo mc jq; do
  command -v "$t" >/dev/null || { echo "missing '$t' — run via: nix develop -c $0"; exit 1; }
done
kubectl config use-context "kind-$CLUSTER" >/dev/null

PF=""
cleanup() { [ -n "$PF" ] && kill "$PF" 2>/dev/null || true; }

# On any failure after the cluster is up, dump why: the controller's
# verdict (.status.message + per-node messages), the live Workflow,
# controller logs, and any failed pod. Without this an Argo-version
# incompatibility just shows "exit code 1" with no cause (esp. in CI).
dump_diagnostics() {
  echo
  printf '\033[1;31m== DIAGNOSTICS (e2e failed, rc=%s)\033[0m\n' "$1"
  echo "--- argo get @latest"
  argo get -n "$NS" @latest -o yaml 2>/dev/null \
    | grep -E '^(  )?(phase|message|name|displayName|templateName|type):' \
    || echo "  (no workflow / argo get failed)"
  echo "--- workflow .status.message"
  kubectl -n "$NS" get wf -o jsonpath='{range .items[*]}{.metadata.name}{"\t"}{.status.phase}{"\t"}{.status.message}{"\n"}{end}' 2>/dev/null \
    || echo "  (none)"
  echo "--- workflow-controller logs (tail)"
  kubectl -n "$NS" logs deploy/workflow-controller --tail=60 2>/dev/null \
    | grep -iE 'error|invalid|fail|reject|unmarshal|unknown field' \
    || echo "  (no error lines)"
  echo "--- failed pods"
  kubectl -n "$NS" get pods -l workflows.argoproj.io/workflow \
    --field-selector=status.phase=Failed -o name 2>/dev/null \
    | while read -r p; do
        echo "  $p"
        kubectl -n "$NS" describe "$p" 2>/dev/null | grep -A3 -iE 'state:|reason:|message:' | sed 's/^/    /'
      done
  printf '\033[1;31m== END DIAGNOSTICS\033[0m\n\n'
}

trap 'rc=$?; [ "$rc" -ne 0 ] && dump_diagnostics "$rc"; cleanup' EXIT

# CI builds the tarball once (build job) and reuses it across the Argo
# matrix: set ATHENA_SKIP_BUILD=1 + ATHENA_TARBALL=<path>.
TARBALL="${ATHENA_TARBALL:-$ROOT/target/athena/${BIN}.tar.gz}"
if [ "${ATHENA_SKIP_BUILD:-0}" = "1" ]; then
  say "using prebuilt tarball: $TARBALL"
else
  say "cross-compile + package (cargo athena build)"
  ( cd "$ROOT" && cargo run -q -p cargo-athena --bin cargo-athena -- athena build \
      --package "$PKG" --bin "$BIN" )
fi
test -f "$TARBALL" || { echo "missing $TARBALL"; exit 1; }

say "upload binary tarball to MinIO"
kubectl -n "$NS" port-forward svc/minio 9000:9000 >/dev/null 2>&1 &
PF=$!
until (exec 3<>/dev/tcp/127.0.0.1/9000) 2>/dev/null; do sleep 1; done
mc alias set athena-e2e http://127.0.0.1:9000 athena athena12345 >/dev/null
mc cp "$TARBALL" \
  athena-e2e/athena-artifacts/athena/bin/e2e/0.1.0/e2e.tar.gz

say "emit WorkflowTemplates + Workflow"
# --with-workflow: this script splits out the runnable Workflow doc and
# `argo submit`s it (the default emit is templates-only).
( cd "$ROOT" && cargo run -q -p cargo-athena --bin cargo-athena -- athena emit \
    --package "$PKG" --bin "$BIN" --with-workflow ) > /tmp/athena-wf.yaml

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

# save_artifact!("result-note", …) pushes to the exact S3 key from the
# port's s3{} block (athena.toml repo), i.e. bucket root key "result-note".
if mc stat athena-e2e/athena-artifacts/result-note >/dev/null 2>&1; then
  echo "save_artifact! object found in MinIO (key: result-note)"
else
  echo "FAIL: save_artifact! object not found at athena-artifacts/result-note"
  exit 1
fi

say "feature-matrix assertions"
# The mega pipeline Succeeding already proves every feature ran; these
# assert the *observable effect* of the trickier ones from pod logs.
WF=$(argo get -n "$NS" @latest -o json | jq -r '.metadata.name')
LOGS=$(argo logs -n "$NS" "$WF" 2>/dev/null || true)
need() { # need <grep-regex> <feature label>
  if printf '%s\n' "$LOGS" | grep -qE "$1"; then
    echo "ok: $2"
  else
    echo "FAIL: $2 — expected /$1/ in workflow logs"; exit 1
  fi
}
need 'echo:nested'                 "nested call (note(echo(..)))"
# fan_out matrix: the OK lines print only AFTER an in-container
# assert_eq! against the exact decoded Rust value — a mis-encoded
# aggregate panics the pod (workflow Error), no quote/format ambiguity.
need 'OK summarize'                "fan_out aggregate: Vec<String> (scalar row)"
need 'OK sum_lens'                 "fan_out aggregate: Vec<i64> (scalar row)"
need 'OK count_true'               "fan_out aggregate: Vec<bool> (scalar row)"
need 'OK collect_bags'             "fan_out aggregate: Vec<Bag> (struct row)"
need 'OK flatten_bags'             "fan_out aggregate: Vec<Vec<Bag>> (array row)"
need 'note:L4'                     "value-if + statement-if (chose left(4))"
need 'id=abc'                      "struct-field access (m.id)"
need 'tagged image busybox:1.36-musl' "image injection (busybox: + tag)"
need 'read e2e-note = payload-42'  "save_artifact! -> load_artifact! roundtrip"
need 'cleanup ran'                 "on_exit_if_root + per-task .on_exit"

# #[workflow(node_selector=…)] must cascade onto every task pod.
SEL=$(kubectl -n "$NS" get pods -l "workflows.argoproj.io/workflow=$WF" \
  -o jsonpath='{.items[0].spec.nodeSelector.kubernetes\.io/arch}' 2>/dev/null)
if [ "$SEL" = "amd64" ]; then
  echo "ok: node_selector cascaded onto task pods (kubernetes.io/arch=amd64)"
else
  echo "FAIL: node_selector not on pods (got '$SEL')"; exit 1
fi

say "CLI gate: container emulate (docker)"
# Run one #[container] under docker exactly as Argo would, from the
# already-built tarball (no S3 needed). Proves the emulate runtime path.
EMU=$( cd "$ROOT" && cargo run -q -p cargo-athena --bin cargo-athena -- \
  athena container emulate cargo-athena-example-e2e-transform \
  --package "$PKG" --bin "$BIN" --tarball "$TARBALL" \
  -a input=ci --skip-artifacts 2>/dev/null | tail -1 )
echo "emulate returned: $EMU"
[ "$EMU" = 'return: "ci-transformed"' ] || {
  echo "FAIL: container emulate — expected 'return: \"ci-transformed\"'"; exit 1; }

say "CLI gate: submit (kube path)"
# `cargo athena submit` against the kube API (kubeconfig current-context
# = this kind cluster). Templates already applied above; --skip-binary-
# check because MinIO is only reachable by its in-cluster DNS here.
SUBWF=$( cd "$ROOT" && cargo run -q -p cargo-athena --bin cargo-athena -- \
  athena submit cargo-athena-example-e2e-finalize-wf \
  --package "$PKG" --bin "$BIN" -n "$NS" --skip-binary-check -y \
  2>/dev/null | tail -1 )
echo "submit created: $SUBWF"
[ -n "$SUBWF" ] || { echo "FAIL: submit printed no workflow name"; exit 1; }
argo wait -n "$NS" "$SUBWF" >/dev/null 2>&1 || true
SUBPHASE=$(argo get -n "$NS" "$SUBWF" -o json 2>/dev/null | jq -r '.status.phase')
[ "$SUBPHASE" = "Succeeded" ] || {
  echo "FAIL: submitted workflow $SUBWF phase=$SUBPHASE"; exit 1; }
echo "ok: submit -> $SUBWF Succeeded"

say "PASS — full e2e green"
