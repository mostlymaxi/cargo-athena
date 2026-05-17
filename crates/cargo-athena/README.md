# cargo-athena

[![crates.io](https://img.shields.io/crates/v/cargo-athena.svg)](https://crates.io/crates/cargo-athena)
[![docs.rs](https://img.shields.io/docsrs/cargo-athena)](https://docs.rs/cargo-athena)

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

Annotate ordinary functions: a `#[workflow]` is a DAG, a `#[container]`
is a step that runs real Rust in a pod, a `#[fragment]` is a plain
helper that carries pod resources.

```rust,ignore
use cargo_athena::{workflow, container};

#[workflow]
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    println!("regular code, got: {a}");
}

fn main() { cargo_athena::entrypoint::<run_foo>(); }
```

This facade is the only crate you depend on; it re-exports the runtime
and proc macros behind one `::cargo_athena` path. Install the
`cargo athena` subcommand (emit / build / run) with
`cargo install cargo-athena-cli`.

- **Feature reference:** the full `#[workflow]` and `#[container]`
  documentation is on [docs.rs](https://docs.rs/cargo-athena) (on the
  `workflow` / `container` macros).
- **Repository & guide:** <https://github.com/mostlymaxi/cargo-athena>
- **License:** MIT OR Apache-2.0
