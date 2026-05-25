# Getting Started

From nothing to a running workflow. Assumes you have an S3-compatible
bucket and a reachable Argo cluster.

## 1. Install cargo-athena

```sh
cargo install cargo-athena                      # the `cargo athena` CLI
cargo add cargo-athena --no-default-features    # the library, in your workflow crate
```

> ⚠️ **Library users: `--no-default-features`.** A workflow crate needs
> only the macros + runtime; the default `cli` feature pulls a heavy
> CLI tree (`kube`, `reqwest`, `tokio`, …) it doesn't use.

## 2. Set up the publish toolchain

`cargo athena publish` cross-compiles your crate as static-musl
binaries for the architectures in your `athena.toml` (Linux pods run
musl). You need three things on the machine that runs `publish`:

```sh
# (a) cargo-zigbuild and the Zig linker
cargo install cargo-zigbuild
pip install ziglang                               # or: brew install zig
                                                  # or: https://ziglang.org/download/

# (b) Rust standard library for each target arch in athena.toml
rustup target add x86_64-unknown-linux-musl
rustup target add aarch64-unknown-linux-musl
```

If something is missing, `cargo athena publish` and `cargo athena
build` will tell you exactly what to install before they start.

**No toolchain needed for `emit` or `submit`** - those don't compile
anything. `cargo athena emit` runs your workflow binary in
emit-mode; `submit` does that plus talks to your cluster. Useful if
you build the tarball on a CI machine and only run `submit` from
elsewhere.

> The repo also ships a Nix flake that installs the full toolchain
> with one command. If you use Nix:
> ```sh
> nix profile install github:mostlymaxi/cargo-athena
> nix develop github:mostlymaxi/cargo-athena   # or just enter a shell
> ```

## 3. Write a tiny pipeline

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

## 4. Add `athena.toml`

Drop this at or above your crate (found by walking up, like
`Cargo.toml`):

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

Full reference: [`athena.toml`](configuration.md).

## 5. Ship it

```sh
cargo athena emit                                    # inspect the YAML, no infra needed
cargo athena publish                                 # cross-compile + upload the binary
cargo athena submit cargo-athena-example-getting-started-pipeline
```

`submit` does the safe-deploy steps for you:

1. type-checks the arguments against the function signature,
2. confirms the binary is uploaded,
3. registers every WorkflowTemplate (asking y/N if any drifted),
4. creates the run and prints its name.

S3 credentials come from the standard AWS env vars or instance-role
identity. See [the CLI page](cli.md#submit) for the `-y`, `--update`,
and `--argo-server` flags.

> **GitOps alternative:** `cargo athena emit | kubectl apply -f -`
> registers the templates; `argo submit --from workflowtemplate/<root>`
> runs them. Names are stable and deterministic.

Want to try one step locally before deploying?
[`cargo athena container emulate`](cli.md#container-emulate) runs a
single `#[container]` under docker/podman exactly as Argo would.

Next: [Core Concepts](concepts.md).
