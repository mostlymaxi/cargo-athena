# Core Concepts

A few ideas explain everything cargo-athena does.

## 1. Templates are types

Each `#[workflow]` / `#[container]` lowers to a unit-struct **type**
implementing an internal `Template` trait. Names, inputs, and the
emitted YAML are derived by the compiler.

Referencing a template type force-links its defining crate; emission
walks the reachable closure from your entrypoint via monomorphic
calls. **No registry, no DCE concern** — nothing uncalled is emitted,
nothing called is missed. Workflows compose across modules and crates
through normal Rust name resolution.

## 2. `#[workflow]` is a statically analyzed DAG

A workflow body is **read, not executed**. Each `let x = t(args);` or
`t(args);` becomes an Argo task; data flow becomes `templateRef`
wiring and DAG edges.

Because the body is *read*, it is also type-checked as ordinary Rust
by a hidden, never-run "ghost" — wrong types, arity, fields, or
calling a non-template are compile errors. Only the lowered shapes
(`let`/call statements, `if`/`else`) are accepted; anything else is a
spanned `compile_error!`. Full details on the
[`#[workflow]`](workflow.md) page.

## 3. `#[container]` runs real Rust in a pod

A container body genuinely executes inside its pod. Arguments arrive
as Argo input parameters; the return is captured as
`outputs.parameters.return` for the next step. I/O is `serde`-bound
at compile time — take and return owned types.

Arguments can also be **spliced into the pod spec**: `image = "repo:"
+ tag` injects an argument into the image (and likewise into
`service_account`, `node_selector`). See
[`#[container]` → Parameter injection](container.md).

## 4. Data flows as JSON parameters

Every value between steps is **JSON-encoded** into an Argo parameter
and decoded by the receiver. The uniformity is what makes the DAG
analysis tractable and what keeps a `String` `"7"` a string on the
wire (never silently parsed as a number).

It also makes more specialised plumbing fall out cleanly: `b.field`
on a binding wires only that field via a JSON path on the producer's
output, and `list.fan_out(|x| step(x))` lowers to Argo `withParam` —
one task invocation per element of the list.

## 5. `#[fragment]` carries pod resources

Pod resources (`host!` mounts, S3 artifact ports) are declared inside
a `#[container]` or `#[fragment]`. A fragment is a normal helper that
runs as real code in the calling pod; every resource it declares is
collected onto each container that transitively calls it — composable
resource decls, no global registry.

## 6. The binary runs in two worlds

The same compiled binary plays two roles:

- **Emit-time**, on your machine — `cargo athena emit` /
  `publish` / `submit` walks the template closure from your
  entrypoint and prints one `WorkflowTemplate` per template.
- **Pod-time**, in Argo — the binary deserializes the step's inputs,
  calls the matching `#[container]` body, and serializes the return.

`cargo athena publish` cross-compiles it static-musl per `athena.toml`
target and uploads it to your S3 `ArtifactRepository`. `emit` adds it
as an input artifact to every container template; a tiny `sh`
bootstrap picks the matching `app-<triple>` and execs it. The image
needs only `sh` and `uname` — distroless works.

---

With these in mind, the reference pages are the details:
[`#[workflow]`](workflow.md), [`#[container]`](container.md),
[the CLI](cli.md), and [`athena.toml`](configuration.md).
