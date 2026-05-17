# Testing

cargo-athena is designed to be tested at three levels — all just
`cargo test`.

## In-process: pin emit + run output

The compiled binary is run in-process and its emitted YAML / run output
is compared to a checked-in expected file. Refresh on intentional
changes:

```sh
cargo test -p cargo-athena-example-smoke                 # check
UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke # refresh expected
```

`examples/smoke` is the broad "all features" fixture; its checked-in
expected YAML catches *any* change to the generated workflow before it
reaches a cluster — a precise, fast regression net. (`examples/e2e` is
the separate crate the live-cluster CI submits; it has no in-process
tests.)

## Compile-fail contracts

The strict `#[workflow]` body contract and the macro guards are pinned
by `trybuild` compile-fail tests: a fixture that *should not* compile,
plus its exact expected error. This is how invariants like "calling a
`#[fragment]` from a `#[workflow]` is an error", "an `if` used as a
value needs an `else`", or "only `String`/number args can be injected
into an attribute" stay true.

## Single step, locally

Run one container's real body with JSON input — no cluster:

```sh
cargo run -q -p cargo-athena-cli -- athena run \
  --template my-crate-transform --input '{"data":"hi","factor":2}'
```

## Full end-to-end against real Argo

Spin a real kind + Argo + MinIO and submit (needs a host Docker/Podman
daemon):

```sh
nix develop -c scripts/deploy.sh     # kind + Argo + MinIO + bucket + RBAC
nix develop -c scripts/e2e-test.sh   # build -> upload -> emit -> submit -> assert Succeeded
nix develop -c scripts/teardown.sh
```

On hosts that block kind cross-node pod networking (e.g. NixOS
default-drop `FORWARD`), set `ATHENA_E2E_SINGLE=1` for a single-node
cluster.

> Conformance to real Argo is guarded **empirically** by this e2e on
> every push to `main`, across the supported Argo matrix — see
> [Supported Argo Versions](argo-versions.md).
