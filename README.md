# cargo-athena

Compile regular Rust into [Argo Workflow](https://argoproj.github.io/workflows/) YAML.

Every `#[workflow]`/`#[container]` becomes a **unit-struct type** that
implements `Template`. That type is the cross-crate **wormhole**: name and
input resolution is done by the compiler (collision-proof), and merely
referencing the type force-links its defining crate. Two worlds, one binary:

- **Emit** — `main` calls `entrypoint::<Root>()`; we walk the closure from
  `Root` and print **one `WorkflowTemplate` document per template**
  (cross-refs via `templateRef`) plus a runnable `Workflow` for `Root`.
  Every container template carries an input artifact (the binary tarball,
  from the `athena.toml` S3 repo) and an arch-resolving bootstrap.
- **Run** — the bootstrap `uname`s, `exec`s the matching static-musl
  `app-<triple>` from the artifact tarball with `--cargo-athena-template
  <name>`; it deserializes inputs, runs the real container body, serializes
  outputs.

### Binary delivery

The single composed binary is cross-compiled (`cargo athena build`, static
musl) for the `athena.toml` target matrix into one `.tar.gz`, stored in an
S3-compatible `ArtifactRepository`. `cargo athena emit` injects that
artifact + a `sh` bootstrap into every container template:

```
inputs.artifacts: [ athena-dist @ /athena/dist.tar.gz, s3{…}, archive:none ]
container: image = #[container(image=…)] or [bootstrap].default_image
           (multi-arch, e.g. busybox:1.36-musl — kubelet picks node arch);
           command/args = sh bootstrap: uname → app-<triple> → exec
```

`#[container(image = "…")]` is arbitrary and per-container by design (run
any image/runtime); it just needs a POSIX `sh`/`tar`/`uname`. Architecture
is resolved at pod start (`uname`), so one template runs on any node arch.
`athena.toml` is required by `cargo athena` (never read in-pod).

Every container template also gets a pod-scoped `emptyDir` volume mounted
at `/athena` (named `athena-work`). All athena paths live under it — the
binary tarball, the `/athena/bin` extraction dir, artifact in/out ports,
and the `result` output — so they're writable on *any* image (distroless /
read-only rootfs) with no `mktemp`/`/tmp` dependency, and the dir is shared
with Argo's init/wait containers for artifact load/collect. `host!`
hostPaths are appended after it.

### Artifact ports (S3 by key)

Inside `#[container]`/`#[fragment]` bodies — addressed by an **exact S3
object key** in the `athena.toml` `[artifact_repository]` (same repo as the
binary). Producer and consumer are fully **decoupled through the bucket**:
no DAG dependency, no `{{tasks.…}}`, no ordering — just a shared key.

- `load_artifact!("key")` / `load_artifact_str!("key")` — Argo pulls the
  exact object `key` from the repo into the pod before it starts; read at
  runtime (`Vec<u8>` / `String`). Missing object → Argo errors (honest).
- `save_artifact!("key", data)` / `save_artifact_str!("key", data)` —
  write `data` at runtime; Argo pushes it to the repo at exactly `key`.

Both emit an Argo artifact with the `s3{}` block (creds from
`athena.toml`) + `archive: none` (raw blob round-trips at `key`). Same
machinery as `host!`: literal-key only, collected by static AST union over
all branches, gated (public form is a `compile_error!` outside
`#[container]`/`#[fragment]`; a `#[workflow]` using one is a hard error),
and propagated through the `#[fragment]` closure. Validated green against
real Argo + MinIO (`scripts/e2e-test.sh`).

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
| `cargo-athena-api` | Hand-owned, curated `serde` subset of the Argo API (no protobuf). Conformance is guarded empirically by the kind e2e. |
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
nix develop                    # Rust 1.95 + musl targets + zig + cluster tooling

# athena.toml (S3 ArtifactRepository + target matrix) is required by
# `cargo athena`; this repo ships one at the root used by the examples.

# Emit the multi-doc WorkflowTemplate stream (artifact + bootstrap injected)
cargo run -q -p cargo-athena-example-basic
cargo run -q -p cargo-athena-cli -- athena emit --package cargo-athena-example-basic

# Cross-compile the static-musl binaries + show the upload key (no compile)
cargo run -q -p cargo-athena-cli -- athena build \
  --package cargo-athena-example-basic --print

# Run one container's real body in-process (templates keyed by <crate>-<fn>)
cargo run -q -p cargo-athena-cli -- athena run \
  --package cargo-athena-example-basic \
  --template cargo-athena-example-basic-run-a-container --input '{"a":"hi"}'

# Cross-crate: the consumer emits the upstream e2e closure too
cargo run -q -p cargo-athena-example-e2e-consumer --bin e2e-consumer

cargo test --workspace
```

## Tested

[![e2e](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml/badge.svg?branch=main)](https://github.com/mostlymaxi/cargo-athena/actions/workflows/e2e.yml)

On every push to `main`, the `e2e` workflow cross-compiles the athena
binary **once** (cached) and then, **in parallel**, stands up a real kind
cluster + Argo + MinIO **per Argo version** and runs the full
`examples/integration` e2e (submit → assert `Succeeded` → assert the
`save_artifact!` object in MinIO). Each row's badge is **live, driven by
that version's matrix job** (overall workflow badge above):

| Argo Workflows | Support | e2e |
|----------------|---------|-----|
| v4.0.5  | maintained (latest minor) | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/GIST_ID/raw/athena-argo-v4.0.5.json) |
| v3.7.14 | maintained (n‑1 minor)    | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/GIST_ID/raw/athena-argo-v3.7.14.json) |
| v3.6.19 | EOL — best-effort         | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/GIST_ID/raw/athena-argo-v3.6.19.json) |
| v3.5.15 | EOL — best-effort         | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/GIST_ID/raw/athena-argo-v3.5.15.json) |
| v3.4.18 | EOL — best-effort         | ![](https://img.shields.io/endpoint?url=https://gist.githubusercontent.com/mostlymaxi/GIST_ID/raw/athena-argo-v3.4.18.json) |

Argo has **no LTS/stable channel**: per its
[release policy](https://github.com/argoproj/argo-workflows/blob/main/docs/releases.md)
only the **two most recent minors** get maintained release branches
(today v4.0 and v3.7); "stable" just means *use the latest patch*. We
test those two as the support target and additionally smoke-test recent
EOL minors best-effort. "Supported" here means *the real Argo at that
version accepts and runs what we emit* — proven by CI, since we hand-own
the API types rather than track a schema.

> **One-time setup for the per-version badges:** create a public gist,
> add repo secrets `GIST_TOKEN` (a PAT with `gist` scope) and
> `BADGE_GIST_ID` (the gist id), and replace `GIST_ID` in the URLs above
> with that id. Until then the overall badge is the live source of truth;
> the per-row badges show "no data" but the workflow step is a no-op and
> never fails the run.

## Kind cluster e2e (real Argo + MinIO)

`scripts/` stands up a 3-node [kind](https://kind.sigs.k8s.io) cluster — 1
control-plane (Argo controller + MinIO, pinned + tolerated) and 2 workers
(workflow pods, kept off control by its taint) — and runs the
`examples/integration` fixture against real Argo:

```sh
nix develop -c scripts/deploy.sh     # cluster + Argo + MinIO + bucket + secret
nix develop -c scripts/e2e-test.sh   # build->upload->emit->submit->assert
nix develop -c scripts/teardown.sh   # kind delete + cleanup
```

`e2e-test.sh` cross-compiles the static-musl binary, uploads the tarball to
MinIO, emits + applies the `WorkflowTemplate`s, `argo submit`s the
`Workflow`, then asserts it **Succeeded**, ran on the **worker** nodes, and
that the `save_artifact!` object landed in MinIO. Needs a host Docker or
Podman daemon. Covers: param data-deps, run-mode (de)serialize, the
`uname` bootstrap + binary-artifact delivery, `host!`/`#[fragment]`
mounts, nested-workflow `templateRef`, and output artifacts.

`deploy.sh` also binds the Argo executor RBAC (`workflowtaskresults`
create/patch) to the workflow ServiceAccount — `namespace-install.yaml`
omits it for the `default` SA, so every step would otherwise 403.

> **Host note:** the 3-node split needs working kind cross-node pod
> networking. On hosts that block it (e.g. NixOS default-drop `FORWARD`),
> set `ATHENA_E2E_SINGLE=1` for `deploy.sh`/`e2e-test.sh` — a 1-node
> cluster that still fully exercises cargo-athena (validated green here).

### ServiceAccount

Workflow pods run as `[defaults].service_account` from `athena.toml`
(default `default`), overridable per template with
`#[container(service_account = "...")]`. Set on every container template
and the runnable `Workflow` so you can bind your own RBAC to it.

### Template extras & workflow body mode

Attribute args are parsed with [`deluxe`]. `#[container]` takes a
`node_selector` map (template-level pod scheduling — Argo has no
per-`DAGTask` nodeSelector):

```rust
#[container(image = "…", node_selector = { "kubernetes.io/arch" = "amd64" })]
```

`#[workflow]` defaults to Argo **`dag:`** (parallel by data-deps).
`#[workflow(steps)]` opts into Argo **`steps:`** — each top-level statement
becomes its own sequential step group (refs become `{{steps.X.outputs
.result}}`), so the imperative reading *is* the execution order.

[`deluxe`]: https://docs.rs/deluxe

## E2E tests (golden, in-process)

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

## Why a hand-owned API (no protobuf, no generated types)

We deliberately **don't** generate types from the official Argo
proto/CRD. We emit a narrow, stable slice of Argo (WorkflowTemplate/
Workflow; templates with container/dag/steps; artifacts/volumes/params/
nodeSelector/SA). Generating the full schema (kopium/proto) cost ~90k LOC
of path-exploded types, opaque holes, and a big-bang refactor for fidelity
we don't functionally need. Instead `cargo-athena-api` is ~30 plain
`#[derive(Serialize, Deserialize)]` structs we own outright, and
**conformance is checked empirically**: `scripts/e2e-test.sh` submits to a
real Argo (pinned **v4.0.5**) + MinIO and asserts success. Adding an Argo
field = adding a field; the e2e catches real drift.
