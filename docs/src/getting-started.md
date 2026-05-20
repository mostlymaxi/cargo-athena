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

> ⚠️ **Library: `--no-default-features`.** A workflow crate needs only
> the macros + runtime. The default `cli` feature pulls a large tree
> (`kube`, `reqwest`, `object_store`, `tokio`, `clap`); disabling it
> keeps your build lean. The installed `cargo athena` binary keeps
> `cli` on, so the subcommand is unaffected.

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

`publish` cross-compiles a static-musl binary per `athena.toml` target
and uploads it to the exact key `emit` references — one step.
Credentials come from `AWS_*` env vars or instance-role identity
(`~/.aws` profiles are **not** read; see
[publish details](cli.md#cargo-athena-publish)):

```sh
cargo athena publish --package my-workflows
# tarball: target/athena/my-workflows.tar.gz
# upload key: athena/my-workflows/0.1.0/my-workflows.tar.gz
# destination: s3://my-bucket/athena/my-workflows/0.1.0/my-workflows.tar.gz (endpoint s3.amazonaws.com)
```

(`cargo athena publish --print` is the dry run — resolve + print the
key without building or uploading. `cargo athena build` packages the
tarball locally *without* uploading, e.g. for a CI artifact.)

## 4. Run it

The recommended path is **`publish` then `submit`** — `submit` does the
`emit` + register steps for you, so the whole loop is just *write Rust
→ `publish` → `submit`*:

```sh
cargo athena submit my-workflows-run-foo -a seed=hello
```

Before creating anything, `submit`:

1. **type-checks** your `-a`/`--input-file` args against the workflow's
   real signature (the same report as `emulate`);
2. confirms the **binary you just `publish`ed** is in the bucket (so
   pods can bootstrap) — `publish` first, then `submit`;
3. **registers + drift-checks** every `WorkflowTemplate` it emits —
   creating missing ones and updating changed ones, after a y/N prompt
   (`-y` to skip prompts, `--update` to force a re-apply);
4. creates the run and prints its **name** on stdout.

No hand-run `emit`, `kubectl apply`, or `argo submit`. It talks to the
Argo Server (`--argo-server`/`$ARGO_SERVER`) or the Kubernetes API via
your kubeconfig — details on [the CLI page](cli.md#submit).

### GitOps / declarative alternative

Prefer to commit the manifests? The template names are **stable and
deterministic**, so `emit` is plain idempotent `kubectl apply` (exactly
what you'd keep in a GitOps repo), and `argo submit --from` runs the
registered root:

```sh
cargo athena emit --package my-workflows | kubectl apply -f -
argo submit --from workflowtemplate/my-workflows-run-foo --watch
```

> Demo shortcut: `cargo athena emit --with-workflow … | kubectl create -f -`
> appends a convenience runnable `Workflow` and fires one run in a
> single command. It's opt-in because that object uses `generateName`
> (non-idempotent, not GitOps-friendly) — the deterministic templates
> are what you keep.

While iterating, `cargo athena emit` alone (no cluster) is the fast
inner loop to eyeball the generated YAML. Next:
[Core Concepts](concepts.md).
