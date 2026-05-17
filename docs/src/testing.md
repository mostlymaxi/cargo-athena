# Testing

Three levels, from fastest to most thorough.

## 1. A single step's real logic

A `#[container]` body is ordinary Rust — `cargo athena run` executes it
in-process with JSON input, exactly as it would in-pod, no cluster:

```sh
cargo athena run --template my-crate-transform \
  --input '{"data":"hi","factor":2}'
```

This is the fast unit test for the *code* inside a step. (You can also
just `#[test]` the plain function directly — it's normal Rust.)

## 2. Guard the generated workflow

`cargo athena emit` is deterministic, so snapshot it and fail CI on an
unintended change — catching DAG/wiring regressions before a cluster
ever sees them:

```sh
cargo athena emit --package my-workflows > expected.yaml   # commit this
# in CI:
diff <(cargo athena emit --package my-workflows) expected.yaml
```

(cargo-athena's own test suite does exactly this with checked-in
expected YAML across a broad "all features" fixture, plus `trybuild`
compile-fail tests pinning the strict `#[workflow]` contract and the
macro guards — `cargo test` in the repo.)

## 3. End-to-end on real Argo

Register the templates and submit on any real Argo + S3 (see
[Getting Started](getting-started.md) step 4) and assert the run
`Succeeded`.

Conformance is not a claim: every push to `main` runs the project's
example workflow against a live Argo + MinIO **per supported version**
and asserts success — see [Supported Argo Versions](argo-versions.md).
The repo's `scripts/{deploy,e2e-test,teardown}.sh` reproduce that
locally (a Docker/Podman daemon required; `nix develop` provides
kind/argo/mc if you use Nix).
