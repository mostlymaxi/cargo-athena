#!/usr/bin/env bash
# Regenerate the official Argo types from the pinned CRDs with kopium.
#
#   nix develop -c crates/cargo-athena-argo/regenerate.sh
#
# Argo's CRD intentionally leaves several recursive/complex nodes opaque
# (`steps: [][]`, `inline` = a recursive Template, …). kopium is strict and
# errors on those, so we first loosen every empty/opaque schema node to
# `x-kubernetes-preserve-unknown-fields` (kopium then emits a permissive
# `serde_json::Value`/map there — those few fields are untyped by design;
# cargo-athena generates `steps`/`dag` itself anyway).
set -euo pipefail

DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

for c in workflows workflowtemplates clusterworkflowtemplates; do
  src="$DIR/crds/argoproj.io_${c}.yaml"
  [ -f "$src" ] || continue
  # Surgical only: (1) the `steps: [][]` shape — give the inner array a
  # permissive item; (2) truly-empty `{}` schema nodes (e.g. `inline`, a
  # recursive Template) — make them preserve-unknown. Nothing else is
  # touched, so the rest stays fully typed.
  yq '
    with(.spec.versions[].schema.openAPIV3Schema;
      ( .. | select(tag=="!!map" and .type=="array" and .items.type=="array") )
        .items.items = {"type":"object","x-kubernetes-preserve-unknown-fields":true}
      | ( .. | select(tag=="!!map" and (keys|length==0)) )
        |= {"type":"object","x-kubernetes-preserve-unknown-fields":true}
    )
  ' "$src" > "$WORK/${c}.yaml"

  out="$DIR/src/${c%s}.rs"  # workflows->workflow, workflowtemplates->workflowtemplate
  kopium -f "$WORK/${c}.yaml" --hide-kube --docs > "$out"
  echo "generated $out ($(wc -l < "$out") lines)"
done
