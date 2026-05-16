# cargo-athena

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

Every `#[workflow]`/`#[container]` becomes a **unit-struct type** that
implements `Template`. That type is the cross-crate **wormhole**: name and
input resolution is done by the compiler (collision-proof), and merely
referencing the type force-links its defining crate. Two worlds, one binary:

- **Emit** — `main` calls `entrypoint::<Root>()`; we walk the closure from
  `Root` and print **one `WorkflowTemplate` document per template**
  (cross-refs via `templateRef`) plus a runnable `Workflow` for `Root`.
- **Run** — Argo invokes the binary with `--cargo-athena-template <name>`; it
  deserializes inputs, runs the real container body, serializes outputs.

```rust
use cargo_athena::{workflow, container, fragment};   // `host!` used path-qualified

#[workflow]                                   // -> a WorkflowTemplate (dag)
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);                       // data dep -> DAG edge + param wiring
}

#[container(image = "ghcr.io/acme/app:latest")]   // -> a WorkflowTemplate (container)
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");   // hostPath volume; compile error outside #[container]/#[fragment]
    load_extra();
    println!("regular code, got: {a}");
}

#[fragment]                                   // plain helper that carries decls
fn load_extra() { let _ = cargo_athena::host!("/var/lib/extra"); }

fn main() { cargo_athena::entrypoint::<run_foo>(); }   // entrypoint = a type
```

Names are `<crate>-<fn>` (override with `#[workflow(name = "...")]`), so
templates are globally unique across crates. A downstream crate just
`use upstream::pipeline;` and calls it inside its own `#[workflow]` — the
whole upstream closure is force-linked and emitted automatically (see
`examples/e2e-consumer`). No `inventory` for templates, no `compose!`.

## Workspace

| Crate | Role |
|---|---|
| `cargo-athena-api` | Argo API types generated from a vendored protobuf subset (prost), serde-able for YAML. Single seam to the full upstream schema. |
| `cargo-athena-core` | Runtime: `Template` trait + `Collector` closure walk, multi-doc emit, `BuildCtx` + `#[fragment]` (`inventory`) host! closure, `host!`. |
| `cargo-athena-macros` | `#[workflow]` / `#[container]` / `#[fragment]` proc macros (emit the type identities). |
| `cargo-athena` | Facade users depend on; the stable `::cargo_athena` path generated code targets. |
| `cargo-athena-cli` | The `cargo athena` subcommand. |
| `examples/basic` | Minimal example + regression test. |
| `examples/e2e` | Broad fixture (lib+bin) incl. an intra-crate cross-module workflow; golden + trybuild tests. |
| `examples/e2e-consumer` | Separate crate importing the e2e fixture's `pipeline` — proves cross-crate composition. |

## Key design decisions

- **Hybrid DAG**: `#[workflow]` bodies are *statically analyzed* (not
  executed) — the seam where this lowers into a functional promise-graph
  for richer control flow later.
- **Resource collection is a static AST union, not a trace.** The attribute
  macro sees every branch's tokens at once, so `host!` declarations are
  collected across *all* `if`/`match`/loop arms. Cross-function cases use
  `#[fragment]` + an `inventory`-resolved transitive closure at emit time.
  No `build.rs`. This is also *correct*, not just tractable: Argo's pod
  spec is fixed before the pod runs, so the union is the only expressible
  semantics. `host!` is literal/const-only by construction.
- **Type-as-wormhole for cross-crate/module reuse.** A template's identity
  is a type implementing `Template`; `<Callee as Template>::ARGO_NAME`
  resolves through normal name resolution (handles `use`/aliases, no
  same-name collisions), and `Template::collect` recurses callees by direct
  monomorphic calls — so the reachable closure is force-linked across
  crates with no `inventory`/DCE games. Emission = the transitive closure
  from the entrypoint; nothing uncalled is emitted.
- **Each template = its own `WorkflowTemplate`**, emitted as a multi-doc
  stream; calls are `templateRef`. Bounds workflow size; the natural
  reuse unit.
- **Single multi-entrypoint binary**, dispatched by `--cargo-athena-template`
  (now keyed by the full `<crate>-<fn>` Argo name).
- **`#[fragment]` host! closure still uses `inventory`** — fragments are
  genuinely *called* by container bodies, so there is no DCE concern there.

## Getting started

```sh
nix develop                    # Rust 1.95 + protoc + buf + cargo tooling

# Emit the multi-doc WorkflowTemplate stream
cargo run -q -p cargo-athena-example-basic
cargo run -q -p cargo-athena-cli -- athena build --package cargo-athena-example-basic

# Run one container's real body in-process (templates keyed by <crate>-<fn>)
cargo run -q -p cargo-athena-cli -- athena run \
  --package cargo-athena-example-basic \
  --template cargo-athena-example-basic-run-a-container --input '{"a":"hi"}'

# Cross-crate: the consumer emits the upstream e2e closure too
cargo run -q -p cargo-athena-example-e2e-consumer --bin e2e-consumer

cargo test --workspace
```

## E2E tests

`examples/e2e` is a stable, broad-coverage fixture (workflows, nested
workflows, containers, fragments, `host!` unioned across `if`/`match`,
multi-dependency DAG). Its tests spawn the real compiled binary and pin
both emit-mode YAML and run-mode output against `tests/golden/`:

```sh
cargo test -p cargo-athena-example-e2e                   # assert vs goldens
UPDATE_EXPECT=1 cargo test -p cargo-athena-example-e2e   # refresh goldens
```

`examples/e2e-consumer` is a *separate crate* that imports the fixture's
`pipeline`; its golden contains the upstream `cargo-athena-example-e2e-*`
templates — the empirical proof the type-wormhole force-links across
crates. `examples/e2e/tests/ui/` (trybuild) pins the compile-fail
contracts: `host!` gating and non-serializable `#[container]` types.

Treat `examples/e2e/src/lib.rs` as a fixture — edits there require
intentionally regenerating the goldens (`UPDATE_EXPECT=1`).

## Build with Nix

```sh
nix build       # ./result/bin/cargo-athena
```

## Swapping in the full Argo schema

Replace `crates/cargo-athena-api/proto/.../argo.proto` with the upstream
`argoproj/argo-workflows` protos (+ k8s deps) and adjust `build.rs` /
`buf.gen.yaml`. No downstream crate changes — everything funnels through
`cargo_athena::api`.
