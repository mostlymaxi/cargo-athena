# Getting Started

From nothing to a running workflow - assuming you have an S3-compatible
bucket and a reachable Argo cluster.

## Install

```sh
cargo install cargo-athena                      # the `cargo athena` subcommand
cargo add cargo-athena --no-default-features    # the library, in your workflow crate

# …or via Nix:
nix profile install github:mostlymaxi/cargo-athena
nix run github:mostlymaxi/cargo-athena -- athena …
```

> ⚠️ **Library users: `--no-default-features`.** A workflow crate needs
> only the macros + runtime; the default `cli` feature pulls a heavy
> CLI tree (`kube`, `reqwest`, `tokio`, …) it doesn't use.

`emit` needs nothing but an `athena.toml`. `publish` also needs the
Zig cross toolchain: `cargo install cargo-zigbuild` +
[`zig`](https://ziglang.org/download/) (`cargo athena build` checks
and tells you what's missing).

## A tiny pipeline

Three containers in a chain - data flow becomes the DAG.
[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/getting-started/src/main.rs)
· [Emitted YAML](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/getting-started/emit.yaml)

```rust,ignore
use cargo_athena::{container, workflow};

#[workflow]
fn pipeline() {
    let raw = fetch("https://example.com/data".to_string());
    let summary = summarize(raw, 3);
    publish(summary);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn fetch(url: String) -> String {
    format!("data-from:{url}")
}

#[container]
fn summarize(data: String, top_n: i64) -> String {
    format!("top-{top_n}:{data}")
}

#[container]
fn publish(report: String) {
    println!("publishing {report}");
}

fn main() {
    cargo_athena::entrypoint!(pipeline);
}
```

Add an [`athena.toml`](configuration.md) at or above your crate (found
by walking up, like `Cargo.toml`):

```toml
[artifact_repository.s3]
endpoint = "s3.amazonaws.com"
bucket   = "my-bucket"
region   = "us-east-1"
access_key_secret = { name = "my-s3", key = "accessKey" }
secret_key_secret = { name = "my-s3", key = "secretKey" }

[bootstrap]
targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]
```

## Ship it

```sh
cargo athena emit                                    # inspect the YAML, no infra needed
cargo athena publish                                 # cross-compile + upload the binary
cargo athena submit cargo-athena-example-getting-started-pipeline
```

`submit` type-checks args, confirms the binary is uploaded, registers
the `WorkflowTemplate`s (y/N on drift), creates the run, and prints its
name. Credentials come from `AWS_*` env vars or instance-role identity.
`-y` skips prompts, `--update` re-applies, `--argo-server`/`$ARGO_SERVER`
selects the REST path - see [the CLI page](cli.md#submit).

> **GitOps alternative:** `cargo athena emit | kubectl apply -f -`
> registers the templates; `argo submit --from workflowtemplate/<root>`
> runs them. Names are stable and deterministic.

Try one step locally before deploying?
[`cargo athena container emulate`](cli.md#container-emulate) runs a
single `#[container]` under docker/podman exactly as Argo would.

Next: [Core Concepts](concepts.md).
