# cargo-athena

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

[![e2e](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml)

**Supported Argo Workflows** — every push to `main` submits the
`examples/e2e` workflow to a real Argo + MinIO per version and
asserts it `Succeeded`; these badges are that live result:

| Argo | Support | e2e |
|---|---|---|
| v4.0.5  | maintained (latest minor) | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v4.0.5.json) |
| v3.7.14 | maintained (n‑1 minor)    | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.7.14.json) |
| v3.6.19 | minimum supported (EOL)   | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.6.19.json) |

Argo ≤ 3.5 is unsupported (its validator can't resolve
`{{tasks.X.outputs.*}}` across the `templateRef` boundary athena relies
on; fixed in Argo 3.6).

## How it works

Annotate ordinary functions. Each `#[workflow]`/`#[container]` becomes a
unit-struct type; referencing that type force-links its crate, so
workflows compose across modules and crates with no registry.

- **emit** — `cargo athena emit` walks the closure from your entrypoint
  and prints one `WorkflowTemplate` per template (cross-refs via
  `templateRef`) plus a runnable `Workflow`.
- **deliver** — `cargo athena build` cross-compiles one static-musl
  `.tar.gz` into the S3 `ArtifactRepository` from `athena.toml`; emit
  injects it + an `sh` bootstrap into every container template.
- **run** — in-pod, the bootstrap `uname`s and execs the matching binary
  with `--cargo-athena-template <name>`: deserialize inputs, run the real
  function body, serialize outputs.

```rust
use cargo_athena::{workflow, container, fragment};   // `host!` used path-qualified

#[workflow]                                   // -> a WorkflowTemplate (dag)
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge + param wiring
}

#[container(image = "ghcr.io/acme/app:latest")]   // -> a WorkflowTemplate (container)
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");   // hostPath; error outside #[container]/#[fragment]
    load_extra();
    println!("regular code, got: {a}");
}

#[fragment]                                   // plain helper that carries decls
fn load_extra() { let _ = cargo_athena::host!("/var/lib/extra"); }

fn main() { cargo_athena::entrypoint::<run_foo>(); }   // entrypoint = a type
```

Also supported: `#[container(image=…, node_selector={…}, service_account="…")]`;
`#[workflow(steps)]` for sequential Argo `steps:` instead of the default
data-dependency `dag:`; and inside `#[container]`/`#[fragment]`,
`host!("/path")` (hostPath) and `load_artifact!`/`save_artifact!("s3-key", …)`
(S3 by literal key, decoupled through the bucket). `athena.toml` (S3 repo
+ target matrix) is required by `cargo athena`.

## Workspace

| Crate | Role |
|---|---|
| `cargo-athena-api` | Hand-owned `serde` subset of the Argo API (no protobuf); conformance guarded by the kind e2e. |
| `cargo-athena-core` | Runtime: `Template` trait, closure walk, multi-doc emit, `BuildCtx`, `host!`. |
| `cargo-athena-macros` | `#[workflow]`/`#[container]`/`#[fragment]` proc macros. |
| `cargo-athena` | Facade users depend on; `tests/` = in-process module/smoke + trybuild compile-fail contracts. |
| `cargo-athena-cli` | The `cargo athena` subcommand. |
| `examples/` | `basic` (minimal), `smoke` (all-features golden fixture), `importing` (cross-module + cross-crate), `e2e` (the kind-e2e crate GHA submits). |

## Getting started

```sh
nix develop

cargo run -q -p cargo-athena-cli -- athena emit  --package cargo-athena-example-basic
cargo run -q -p cargo-athena-cli -- athena build --package cargo-athena-example-basic --print
cargo run -q -p cargo-athena-cli -- athena run \
  --package cargo-athena-example-basic \
  --template cargo-athena-example-basic-run-a-container --input '{"a":"hi"}'

cargo test --workspace
```

## Testing

Golden tests run the compiled binary in-process and pin emit + run output:

```sh
cargo test -p cargo-athena-example-e2e                   # vs goldens
UPDATE_EXPECT=1 cargo test -p cargo-athena-example-e2e   # refresh goldens
```

Full e2e against real Argo + MinIO (needs a host Docker/Podman daemon):

```sh
nix develop -c scripts/deploy.sh     # kind + Argo + MinIO + bucket + RBAC
nix develop -c scripts/e2e-test.sh   # build -> upload -> emit -> submit -> assert
nix develop -c scripts/teardown.sh
```

On hosts that block kind cross-node pod networking (e.g. NixOS
default-drop `FORWARD`), set `ATHENA_E2E_SINGLE=1` for a 1-node cluster.

## Build

```sh
nix build       # ./result/bin/cargo-athena
```
