# Testing

cargo-athena gives you a fast inner loop without a cluster, plus
guard rails before code reaches one. Four levels, fastest to most
thorough.

## 1. Unit-test the container body

A `#[container]` body is ordinary Rust, so call it from a regular
`#[test]`:

```rust,ignore
#[container]
fn summarize(data: String, top_n: i64) -> String {
    format!("top-{top_n}:{data}")
}

#[test]
fn summarize_picks_top_n() {
    assert_eq!(summarize("hello".into(), 3), "top-3:hello");
}
```

No harness, no infrastructure. This is the right test for "does my
business logic do the right thing on these inputs?".

## 2. Run the step like Argo would (`emulate`)

For "does the step run correctly *in its container*?",
`cargo athena emulate` is the fast inner loop. It runs one
`#[container]` under docker or podman with the same image, the same
injected bootstrap, the same parameter env, the `/athena` scratch
dir, `host!` binds, and S3 artifact ports.

```sh
cargo athena emulate ./my-workflow -w transform -a data=hi -a factor=2
```

By default it **pulls the deployed binary from S3**, so what you
emulate is what's actually live. `--build` packages a fresh local
musl binary instead; `--tarball F` uses one verbatim.

Arguments are type-checked against the real function signature
before anything launches, so wrong types fail fast with a clear
message instead of a serde panic inside the pod.

**Not emulated:** anything Kubernetes-specific (ServiceAccount, RBAC,
`nodeSelector`, podSpecPatch). See [the CLI page](cli.md#emulate) for
the full list and flags.

## 3. Snapshot the emitted YAML

`cargo athena emit` is deterministic, so snapshot it and fail CI on
unintended changes:

```sh
# Commit a baseline
cargo athena emit --package my-workflows > tests/golden/emit.yaml

# In CI, fail loud on any diff
diff <(cargo athena emit --package my-workflows) tests/golden/emit.yaml
```

This catches DAG / wiring / parameter regressions before a cluster
ever sees them. cargo-athena's own test suite does this across a
broad "all features" fixture (see
[`examples/smoke/tests/golden/`](examples.md#smoke)) plus
`trybuild` compile-fail tests pinning the strict body grammar and
macro guards.

## 4. End-to-end on real Argo

For full conformance, submit to a real Argo + S3:

```sh
cargo athena publish
cargo athena submit my-crate-pipeline -a seed=hi
```

Wait for the run, assert it `Succeeded`. The project's own GitHub
Actions matrix does exactly this on every push to `main` against
three Argo versions (4.0.8 / 3.7.17 / 3.6.19) and the badges in
[Supported Argo Versions](argo-versions.md) are that live result.

To reproduce the cluster locally:

```sh
scripts/deploy.sh && scripts/e2e-test.sh && scripts/teardown.sh
```

You need a Docker or Podman daemon. `nix develop` provides
kind / argo / mc if you use Nix.
