# cargo-athena

[![clippy](https://github.com/mostlymaxi/cargo-athena/actions/workflows/clippy.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/clippy.yml)
[![test](https://github.com/mostlymaxi/cargo-athena/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/test.yml)
[![e2e](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml)
[![crates.io](https://img.shields.io/crates/v/cargo-athena.svg)](https://crates.io/crates/cargo-athena)
[![docs.rs](https://img.shields.io/docsrs/cargo-athena)](https://docs.rs/cargo-athena)

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

```sh
cargo add cargo-athena              # the library
cargo install cargo-athena-cli      # the `cargo athena` subcommand
```

📖 **[Documentation](https://mostlymaxi.github.io/cargo-athena/)** — from
zero to adept (the same `#[workflow]`/`#[container]` reference is also in
`cargo doc`).

**Supported Argo Workflows** — every push to `main` submits the
`examples/e2e` workflow to a real Argo + MinIO per version and
asserts it `Succeeded`; these badges are that live result:

| Argo | Support | e2e |
|---|---|---|
| v4.0.5  | maintained (latest minor) | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v4.0.5.json) |
| v3.7.14 | maintained (n‑1 minor)    | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.7.14.json) |
| v3.6.19 | minimum supported (EOL)   | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.6.19.json) |

Argo ≤ 3.5 is unsupported - older versions *may* still work, use at your own risk!

## Getting Started

Annotate ordinary functions. A `#[workflow]` is a DAG; a `#[container]`
is a step that runs real Rust in a pod; a `#[fragment]` is a plain
helper that carries pod resources.

```rust
use cargo_athena::{workflow, container, fragment};

#[workflow]
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge + param wiring
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");   // hostPath mount
    load_extra();
    println!("regular code, got: {a}");
}

#[fragment]
fn load_extra() { let _ = cargo_athena::host!("/var/lib/extra"); }

fn main() { cargo_athena::entrypoint::<run_foo>(); }   // entrypoint = a type
```

Each `#[workflow]`/`#[container]` compiles to its own Argo
`WorkflowTemplate`, cross-referenced by `templateRef` (referencing a
template's type force-links its crate, so workflows compose across
modules and crates with no registry). `cargo athena build`
cross-compiles one static-musl binary into the S3 `ArtifactRepository`
from `athena.toml`; `emit` injects that binary plus a tiny `sh`
bootstrap into every container template, so in-pod each step pulls the
binary, picks its arch, and runs the right function — deserialize
inputs, run the body, serialize outputs.

```sh
nix develop

cargo run -q -p cargo-athena-cli -- athena emit  --package cargo-athena-example-basic
cargo run -q -p cargo-athena-cli -- athena build --package cargo-athena-example-basic --print
cargo run -q -p cargo-athena-cli -- athena run \
  --package cargo-athena-example-basic \
  --template cargo-athena-example-basic-run-a-container --input '{"a":"hi"}'
```

**Full feature reference:** [`WORKFLOW.md`](WORKFLOW.md) (every
`#[workflow]` arg + call form) and [`CONTAINER.md`](CONTAINER.md) (every
`#[container]` arg, `#[fragment]`, and in-pod macro). The same content is
on the macros in `cargo doc`.

## Testing

In-process tests run the compiled binary and pin emit + run output:

```sh
cargo test -p cargo-athena-example-smoke                 # check
UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke # refresh expected output
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

## License

Licensed under either of

- Apache License, Version 2.0 ([LICENSE-APACHE](LICENSE-APACHE) or
  <http://www.apache.org/licenses/LICENSE-2.0>)
- MIT license ([LICENSE-MIT](LICENSE-MIT) or
  <http://opensource.org/licenses/MIT>)

at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the Apache-2.0
license, shall be dual licensed as above, without any additional terms
or conditions.
