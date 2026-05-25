# Supported Argo Versions

Every push to `main` submits the real `examples/e2e` workflow to a live
Argo + MinIO **per version** and asserts it `Succeeded`. The support
matrix is therefore not a claim - it is a continuously verified result.

| Argo | Support |
|---|---|
| **v4.0.5** | maintained (latest minor) |
| **v3.7.14** | maintained (n‑1 minor) |
| **v3.6.19** | minimum supported (EOL, hard-gated) |

Argo Workflows maintains the two most recent minors; cargo-athena tracks
that plus the minimum that still works. All three are **blocking** CI
jobs (no `continue-on-error`).

## Why ≤ 3.5 is unsupported

cargo-athena emits **one `WorkflowTemplate` per template**, wired via
`templateRef`. Argo's submit-time validator before 3.6 cannot resolve
`{{tasks.X.outputs.*}}` across a `templateRef` boundary, so any
multi-step workflow fails instantly with
`failed to resolve {{tasks.a.outputs.…}}`.

This is intrinsic to the one-template-per-function model and was **fixed
in Argo 3.6** - the emitted YAML is correct and passes 3.6/3.7/4.0
unchanged. Older versions *may* still work for trivial cases; use at
your own risk.

## Live badges

GitHub has no per-matrix-job badge, so each matrix job publishes its
pass/fail to a gist and the README renders
[shields.io](https://shields.io) endpoint badges from it - the badges at
the top of the README are that live e2e result.
