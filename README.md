# cargo-athena

[![clippy](https://github.com/mostlymaxi/cargo-athena/actions/workflows/clippy.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/clippy.yml)
[![test](https://github.com/mostlymaxi/cargo-athena/actions/workflows/test.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/test.yml)
[![e2e](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml)
[![crates.io](https://img.shields.io/crates/v/cargo-athena.svg)](https://crates.io/crates/cargo-athena)
[![docs.rs](https://img.shields.io/docsrs/cargo-athena)](https://docs.rs/cargo-athena)

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

```rust
use cargo_athena::{workflow, container};

#[workflow]
fn pipeline() {
    let raw = fetch("https://example.com/data".to_string());
    let clean = transform(raw, 3);
    publish(clean);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn transform(data: String, factor: i64) -> String {
    format!("{data} x{factor}")          // this runs in the pod
}

fn main() { cargo_athena::entrypoint!(pipeline); }
```

That `#[workflow]` becomes one Argo `WorkflowTemplate` per function,
wired by data dependencies. `cargo athena publish` cross-compiles your
crate as a static-musl binary and uploads it to S3; an injected
bootstrap fetches the right arch in-pod and runs the matching function.
Workflows compose across modules and crates; you never write or
generate YAML.

## Install

```sh
cargo add cargo-athena --no-default-features   # the library
cargo install cargo-athena                     # the `cargo athena` CLI

# …or via Nix:
nix profile install github:mostlymaxi/cargo-athena
nix run github:mostlymaxi/cargo-athena -- athena …
```

> [!IMPORTANT]
> **Library users: keep `default-features = false`.** A workflow crate
> needs only the proc macros + runtime; the default `cli` feature pulls
> a heavy CLI tree (`kube`, `reqwest`, `tokio`, …) it doesn't use.

## Docs

📖 **[Full documentation](https://mostlymaxi.github.io/cargo-athena/)**
covers [getting started](https://mostlymaxi.github.io/cargo-athena/getting-started.html),
[core concepts](https://mostlymaxi.github.io/cargo-athena/concepts.html),
the [cookbook](https://mostlymaxi.github.io/cargo-athena/cookbook.html),
and [troubleshooting](https://mostlymaxi.github.io/cargo-athena/troubleshooting.html).

The complete macro reference lives on [docs.rs](https://docs.rs/cargo-athena)
and in this repo as [`WORKFLOW.md`](WORKFLOW.md) and [`CONTAINER.md`](CONTAINER.md).

## Supported Argo Workflows

Every push to `main` submits the `examples/e2e` workflow to a real
Argo + MinIO per version and asserts it `Succeeded`. These badges are
that live result:

| Argo | Support | e2e |
|---|---|---|
| v4.0.5  | maintained (latest minor) | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v4.0.5.json) |
| v3.7.14 | maintained (n‑1 minor)    | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.7.14.json) |
| v3.6.19 | minimum supported (EOL)   | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/6c34ed5be0444407c50ccf4597acba1f/raw/athena-argo-v3.6.19.json) |

Argo ≤ 3.5 is unsupported (cross-templateRef output resolution was
fixed in 3.6); older versions may still work for trivial cases, use
at your own risk.

## Contributing

```sh
nix develop            # toolchain + zig/cargo-zigbuild + kubectl/argo/mc
cargo test --workspace # unit + golden + trybuild compile-fail
nix build              # -> ./result/bin/cargo-athena

# full e2e on real kind + Argo + MinIO (needs a Docker/Podman daemon):
scripts/deploy.sh && scripts/e2e-test.sh && scripts/teardown.sh
```

The dev shell pulls a prebuilt Rust toolchain from
`nix-community.cachix.org` (fenix). Trusted Nix users get this
automatically; otherwise pass `--accept-flake-config`.

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or
[MIT](LICENSE-MIT), at your option.

Unless you explicitly state otherwise, any contribution intentionally
submitted for inclusion in the work by you, as defined in the
Apache-2.0 license, shall be dual licensed as above without any
additional terms or conditions.
