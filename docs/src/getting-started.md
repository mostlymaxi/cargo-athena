# Getting Started

From nothing to a running workflow in ~5 minutes (assuming you have an
S3-compatible bucket and Argo reachable somewhere).

## Install

```sh
cargo install cargo-athena                     # the `cargo athena` subcommand
cargo add cargo-athena --no-default-features    # the library, in your workflow crate

# …or get the CLI via the Nix flake:
nix profile install github:mostlymaxi/cargo-athena   # install
nix run github:mostlymaxi/cargo-athena -- athena …   # one-off
```

> ⚠️ **Library: `--no-default-features`.** Your workflow crate needs
> only the macros + runtime. The default `cli` feature (shipped by
> `cargo install`) drags in the whole CLI tree — `kube`/`k8s-openapi`,
> `reqwest`, `object_store`, `tokio`, `clap`. Disabling default
> features keeps your dependency graph (and build) lean; the
> `cargo athena` *binary* keeps `cli` on, so nothing about the
> subcommand changes.

`emit` needs nothing but an `athena.toml`. `build` additionally needs
the Zig cross toolchain — `cargo install cargo-zigbuild` plus
[`zig`](https://ziglang.org/download/) (`cargo athena build` checks for
both and tells you exactly what to install if either is missing).

> Nix users: `nix develop` in the repo provides all of the above
> (toolchain, zig/cargo-zigbuild, kubectl/argo/mc). It's a convenience,
> not a requirement — none of the commands below assume it.

## Your first workflow

```rust,ignore
use cargo_athena::{workflow, container, fragment};

#[workflow]                                   // -> a WorkflowTemplate (dag)
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge + wiring
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    let _cfg = cargo_athena::host!("/etc/myapp");  // hostPath mount
    load_extra();
    println!("regular code, got: {a}");
}

#[fragment]                                   // plain helper that carries decls
fn load_extra() { let _ = cargo_athena::host!("/var/lib/extra"); }

fn main() { cargo_athena::entrypoint::<run_foo>(); }   // entrypoint = a type
```

- The **entrypoint is a type** (`run_foo`), not a string — referencing
  it force-links the whole reachable closure of templates.
- `run_a_container`'s body is **real code** that runs in the pod;
  `host!` / `load_artifact!` / `save_artifact!` declare its pod
  resources.

Add an [`athena.toml`](configuration.md) somewhere at or above your
crate (it is found by walking up, like `Cargo.toml`; or pass
`cargo athena -c path/to/athena.toml …`). Minimal:

```toml
[artifact_repository.s3]
endpoint = "s3.amazonaws.com"
bucket   = "my-bucket"
region   = "us-east-1"
access_key_secret = { name = "my-s3", key = "accessKey" }
secret_key_secret = { name = "my-s3", key = "secretKey" }

[artifact]
key = "athena/{crate}/{version}/{bin}.tar.gz"

[bootstrap]
targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
```

## 1. Check the YAML — no infra needed

```sh
cargo athena emit --package my-workflows
```

Relays one `WorkflowTemplate` per reachable template — stable,
deterministic names, the artifact you register and version. No cluster,
no S3, no cross-build — the fast inner loop while you shape the DAG.

## 2. (optional) Emulate one step locally

Run a single `#[container]` under docker/podman exactly as Argo would —
its image, the injected bootstrap, `ATHENA_PARAM_*` env:

```sh
cargo athena container emulate my-workflows-run-a-container -a a=hi
```

The positional name is the Argo name: `<crate>-<fn>` kebab-case
(override with `#[container(name = "…")]`). By default the *deployed*
binary is pulled from S3; `--build` packages a local one instead. See
[the CLI page](cli.md#container-emulate).

## 3. Build & upload the binary

`build` cross-compiles a static-musl binary per `athena.toml` target,
packages them into one tarball, and **prints the exact upload
destination**:

```sh
cargo athena build --package my-workflows
# …
# tarball: target/athena/my-workflows.tar.gz
# upload key: athena/my-workflows/0.1.0/my-workflows.tar.gz
# destination: s3://my-bucket/athena/my-workflows/0.1.0/my-workflows.tar.gz (endpoint s3.amazonaws.com)
```

Upload that tarball to exactly that key — pick whichever client you
have (the `s3://…` path is what `build` printed):

```sh
s3cmd put target/athena/my-workflows.tar.gz \
  s3://my-bucket/athena/my-workflows/0.1.0/my-workflows.tar.gz

# or:  aws s3 cp target/athena/my-workflows.tar.gz s3://my-bucket/athena/my-workflows/0.1.0/my-workflows.tar.gz
# or:  mc cp      target/athena/my-workflows.tar.gz  myalias/my-bucket/athena/my-workflows/0.1.0/my-workflows.tar.gz
```

(`cargo athena build --print` does the dry run — resolve + print the
key without building or uploading.)

## 4. Register the templates and run it

`emit` injects that tarball + a tiny `sh` bootstrap into every container
template. Register the `WorkflowTemplate`s — they have stable,
deterministic names, so this is plain idempotent `kubectl apply` (and
exactly what you'd commit to a GitOps repo):

```sh
cargo athena emit --package my-workflows | kubectl apply -f -
```

Then trigger a run from the registered root template
(`<crate>-<entrypoint>`); `--from` mints a fresh run each time, so you
re-run with just this — no re-emit, no `generateName` object to manage:

```sh
argo submit --from workflowtemplate/my-workflows-run-foo --watch
```

> Demo shortcut: `cargo athena emit --with-workflow … | kubectl create -f -`
> appends a convenience runnable `Workflow` and fires one run in a
> single command. It's opt-in because that object uses `generateName`
> (non-idempotent, not GitOps-friendly) — the deterministic templates
> are what you keep.

That is the whole loop: **write Rust → `emit` to iterate → `build` +
upload → `emit | kubectl apply` → `argo submit --from` to run/re-run.**
Next: [Core Concepts](concepts.md).
