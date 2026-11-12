# Testing

cargo-athena gives you a fast inner loop without a cluster, plus
guard rails before code reaches one. Three levels, fastest to most
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

## 2. Snapshot the emitted YAML

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

## 3. End-to-end on real Argo

For full conformance, submit to a real Argo + S3:

```sh
cargo athena publish
cargo athena submit my-crate-pipeline -a seed=hi
```

Wait for the run, assert it `Succeeded`.

To run a local kind cluster with Argo + MinIO from a clone of this
repo:

```sh
scripts/deploy.sh && scripts/e2e-test.sh && scripts/teardown.sh
```

You need a Docker or Podman daemon. `nix develop` provides
kind / argo / mc if you use Nix.
