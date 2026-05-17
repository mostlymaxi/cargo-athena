# Testing

Three levels, from fastest to most thorough.

## 1. A single step's real logic

A `#[container]` body is ordinary Rust, so the fastest test is just a
plain `#[test]` calling the function directly — no harness, no infra.

For the *step as it actually runs in-pod* (its image, the injected
bootstrap, `ATHENA_PARAM_*` env, `host!`/artifact ports), use
`cargo athena container emulate` — docker/podman locally, exactly as
Argo would, with the deployed binary pulled from S3:

```sh
cargo athena container emulate my-crate-transform -a data=hi -a factor=2
```

See [the CLI page](cli.md#container-emulate) for `--build` (local
binary) and the Kubernetes-context limitations.

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
