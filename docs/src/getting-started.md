# Getting Started

## Prerequisites

- **Rust** (the repo pins a toolchain via `rust-toolchain.toml`; the nix
  dev shell provides it).
- For `emit`: an [`athena.toml`](configuration.md) (it bakes the
  artifact source into the YAML) — but no cluster, S3, or cross-build.
- For `build` / running on a cluster: also an **S3-compatible bucket**
  (real S3, MinIO, …) and a **musl cross toolchain** — both provided by
  the nix dev shell (`cargo-zigbuild` + `zig`).

The quickest way to get a correct environment is the dev shell:

```sh
nix develop
```

## Your first workflow

Add the facade crate and annotate ordinary functions:

```rust,ignore
use cargo_athena::{workflow, container, fragment};

#[workflow]                                   // -> a WorkflowTemplate (dag)
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge + wiring
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");   // hostPath mount
    load_extra();
    println!("regular code, got: {a}");
}

#[fragment]                                   // plain helper that carries decls
fn load_extra() { let _ = cargo_athena::host!("/var/lib/extra"); }

fn main() { cargo_athena::entrypoint::<run_foo>(); }   // entrypoint = a type
```

Two things to notice:

- The **entrypoint is a type** (`run_foo`), not a string. Referencing it
  is what force-links the whole reachable closure of templates.
- `run_a_container`'s body is **real code**. It runs in the pod;
  `host!` (and `load_artifact!`/`save_artifact!`) declare the pod
  resources it needs.

## See the YAML (`emit`)

`emit` runs your binary in emit-mode and relays the multi-document
`WorkflowTemplate` stream plus a runnable `Workflow`:

```sh
cargo run -q -p cargo-athena-cli -- athena emit --package cargo-athena-example-basic
```

No cluster, no S3, no cross-build — just an
[`athena.toml`](configuration.md). The fastest feedback loop while you
shape a workflow.

## Run one step locally (`run`)

You can execute a single container's body in-process, exactly as it
would run in-pod, by feeding it JSON input:

```sh
cargo run -q -p cargo-athena-cli -- athena run \
  --package cargo-athena-example-basic \
  --template cargo-athena-example-basic-run-a-container \
  --input '{"a":"hi"}'
```

`--template` is the Argo name, `<crate>-<fn>` in kebab-case (override
with `#[container(name = "…")]`).

## Ship it (`build`)

`build` cross-compiles a static-musl binary per target in
[`athena.toml`](configuration.md), packages them into one `.tar.gz`, and
prints the upload key (use `--print` for a dry run / CI):

```sh
cargo run -q -p cargo-athena-cli -- athena build \
  --package cargo-athena-example-basic --print
```

`emit` then injects that tarball — plus a tiny `sh` bootstrap — into
every container template, so each pod pulls the binary, selects its
architecture, and runs your function.

That is the whole loop: **write Rust → `emit` to iterate → `build` +
submit to run.** Next: [Core Concepts](concepts.md).
