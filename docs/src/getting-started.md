# Getting Started

From nothing to a running workflow. Assumes you have an S3-compatible
bucket and a reachable Argo cluster.

## 1. Install cargo-athena

```sh
cargo install cargo-athena                      # the `cargo athena` CLI
```

You'll add the library to your workflow crate in step 3 via
`cargo athena init`; or you can add it to an existing crate with
`cargo add cargo-athena --no-default-features`.

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

Run `cargo athena doctor` to verify every prereq is green before you
try to publish:

```sh
cargo athena doctor
# checks cargo-zigbuild, zig, rustup targets, athena.toml, AWS creds
```

**No toolchain needed for `emit` or `submit`** - those don't compile
anything. Useful if you build the tarball on CI and only `submit` from
elsewhere.

> The repo also ships a Nix flake that installs the full toolchain
> with one command. If you use Nix:
> ```sh
> nix profile install github:mostlymaxi/cargo-athena
> nix develop github:mostlymaxi/cargo-athena   # or just enter a shell
> ```

## 3. Scaffold a workflow crate

```sh
cargo athena init my-pipeline       # prompts for bucket/endpoint/region
cd my-pipeline
```

This writes a runnable starter (a one-container `hello` pipeline)
plus an `athena.toml` skeleton:

```
my-pipeline/
  Cargo.toml          # cargo-athena dep, default-features = false
  src/main.rs         # #[workflow] pipeline() { hello("world") }
  athena.toml         # your S3 bucket + bootstrap targets
```

Pass `-y` to accept defaults without prompts, or flags like
`--bucket`, `--endpoint`, `--region` for a fully scripted run.

> Already have a crate? Add cargo-athena with
> `cargo add cargo-athena --no-default-features` and skip `init`.
> See [`athena.toml`](configuration.md) for the config format.

## 4. Write your pipeline

Open `src/main.rs` and replace the `hello` starter with whatever you
need. A typical multi-step pipeline:

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

Data flow becomes the DAG. See [Core Concepts](concepts.md) and the
[Cookbook](cookbook.md) for what else you can do.

## 5. Ship it

```sh
cargo athena emit                # inspect the YAML, no infra needed
cargo athena publish             # cross-compile + upload the binary
cargo athena submit my-pipeline-pipeline
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
[`cargo athena emulate`](cli.md#emulate) runs a single `#[container]`
under docker / podman exactly as Argo would.

Next: [Core Concepts](concepts.md).
