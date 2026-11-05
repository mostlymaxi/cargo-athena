# Supported Argo Versions

Every push to `main` submits the real `examples/e2e` workflow to a live
Argo + MinIO **per version** and asserts it `Succeeded`. The support
matrix is therefore not a claim - it is a continuously verified result.

| Argo | Support |
|---|---|
| **v4.1.6** | maintained (latest minor) |
| **v4.0.15** | maintained (n-1 minor) |

Argo Workflows maintains the two most recent minors, and cargo-athena
supports exactly those. Both are **blocking** CI jobs (no
`continue-on-error`).

## Older versions

Argo 3.x is not supported.

- **3.6 and 3.7** are not tested. Most workflows may still run, but on
  3.6 any task whose arguments are larger than about 128 KB fails with
  `argument list too long`.
- **3.5 and older** cannot run a multi-step cargo-athena workflow. The
  submit fails at once with `failed to resolve {{tasks.X.outputs.…}}`.

## Live badges

GitHub has no per-matrix-job badge, so each matrix job publishes its
pass/fail to a gist and the README renders
[shields.io](https://shields.io) endpoint badges from it - the badges at
the top of the README are that live e2e result.
