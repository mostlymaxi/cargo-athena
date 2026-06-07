# cargo-athena

**Write a normal Rust program. Get an Argo Workflow.**

cargo-athena compiles ordinary, annotated Rust into
[Argo Workflows](https://argoproj.github.io/workflows/) YAML - and ships
your compiled binary so every step runs your *real* code.

```rust,ignore
use cargo_athena::{workflow, container};

#[workflow]
fn pipeline() {
    let raw = fetch("https://example.com/data".to_string());   // a #[container]
    let clean = transform(raw, 3);
    publish(clean);                                            // a #[container]
}

#[container(image = "ghcr.io/acme/app:latest")]
fn transform(data: String, factor: i64) -> String {
    format!("{data} x{factor}")          // this actually runs in the pod
}
```

That `#[workflow]` becomes an Argo `WorkflowTemplate` whose DAG wires
`fetch → transform → publish` by their data dependencies. `transform`
becomes a container template of its own. In-pod, your binary
deserializes `data` and `factor`, runs the function body, and
serializes the result for the next step.

```yaml
# `cargo athena emit` → one WorkflowTemplate per fn, wired by name:
kind: WorkflowTemplate              # the #[workflow] is a DAG
metadata: { name: app-pipeline }
spec:
  templates:
    - dag:
        tasks:
          - { name: fetch }
          - { name: transform, dependencies: [fetch] }   # data dep → edge
          - { name: publish,   dependencies: [transform] }
# …plus one WorkflowTemplate per #[container]: your image, your binary,
# and a tiny bootstrap that runs the right function.
```

## Why

- **No YAML.** The workflow *is* the program. Refactor with the
  compiler, not a templating language.
- **Type-checked data flow.** Passing the wrong type between steps, a
  missing struct field, or consuming a step that returns nothing is a
  **compile error** - caught long before a cluster ever sees it.
- **Composable.** A workflow is a Rust type. Reference it from another
  crate and it comes along automatically: workflows compose across
  modules and crates with no registry or hand-run codegen.
- **Real Rust in any image.** Your binary is fetched at runtime, so
  each step runs your Rust *on top of* any image you pick - a vendor's
  `postgres:16`, a CUDA base, your team's tooling image - with no
  custom Dockerfile per step. The image just needs `sh` and `uname`.

## How it fits together

| You write | cargo-athena produces |
|---|---|
| `#[workflow] fn` | an Argo `WorkflowTemplate` (a DAG, or sequential `steps`) |
| `#[container] fn` | an Argo `WorkflowTemplate` (a container step) that runs your real Rust |
| `#[fragment] fn` | a plain helper that carries pod resources into its callers |
| `fn main()` | the entrypoint into your workflow binary |

Ship and run it in two commands: `cargo athena publish` (cross-compile
and upload the binary), then `cargo athena submit <workflow>` (registers
the templates and starts the run) - see the [CLI](cli.md).

Read [Getting Started](getting-started.md) to go hands-on, then
[Core Concepts](concepts.md) for the mental model. The
[`#[workflow]`](workflow.md) and [`#[container]`](container.md) pages are
the complete feature reference.
