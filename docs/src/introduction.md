# cargo-athena

**Write a normal Rust program. Get an Argo Workflow.**

cargo-athena compiles ordinary, annotated Rust into
[Argo Workflows](https://argoproj.github.io/workflows/) YAML — and ships
your compiled binary so every step runs your *real* code.

```rust,ignore
use cargo_athena::{workflow, container};

#[workflow]
fn pipeline() {
    let raw = fetch("https://example.com/data".to_string());
    let clean = transform(raw, 3);
    publish(clean);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn transform(data: String, factor: i64) -> String {
    format!("{data} x{factor}")          // this actually runs in the pod
}
```

That `#[workflow]` becomes an Argo `WorkflowTemplate` whose DAG wires
`fetch → transform → publish` by their data dependencies. `transform`
becomes a container template; in-pod, your binary deserializes `data`
and `factor`, runs the function body, and serializes the result for the
next step.

## Why

- **No YAML.** The workflow *is* the program. Refactor with the
  compiler, not a templating language.
- **Type-checked data flow.** Passing the wrong type between steps, a
  missing struct field, or consuming a step that returns nothing is a
  **compile error** — caught long before a cluster ever sees it.
- **Composable.** A workflow is a Rust type. Referencing it from another
  crate force-links it; workflows compose across modules and crates with
  no registry, no `build.rs`, no codegen step you run by hand.
- **One binary.** `cargo athena publish` cross-compiles a single
  static-musl binary into your artifact bucket; every container pulls it
  and runs the right function. You ship one thing.

## How it fits together

| You write | cargo-athena produces |
|---|---|
| `#[workflow] fn` | an Argo `WorkflowTemplate` (a DAG, or sequential `steps`) |
| `#[container] fn` | an Argo `WorkflowTemplate` (a container step) that runs your real Rust |
| `#[fragment] fn` | a plain helper that carries pod resources into its callers |
| `fn main()` | the entrypoint — and the single multi-step binary |

Read [Getting Started](getting-started.md) to go hands-on, then
[Core Concepts](concepts.md) for the mental model. The
[`#[workflow]`](workflow.md) and [`#[container]`](container.md) pages are
the complete feature reference.
