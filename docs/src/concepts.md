# Core Concepts

A few ideas explain everything cargo-athena does.

## 1. Templates are types

Each `#[workflow]` / `#[container]` lowers to a Rust type. Referencing
that type from another module or crate force-links its definition;
emission walks the closure of every template you reach from your
entrypoint.

There is no registry to keep in sync. Workflows compose across
modules and crates through normal Rust name resolution.

## 2. `#[workflow]` is a statically analyzed DAG

A workflow body is **read, not executed**. Each `let x = t(args);`
or `t(args);` becomes a step; data flow becomes the DAG edges.

Because the body is *read*, it is also type-checked as ordinary
Rust. Wrong types, wrong arity, missing fields, or calling a
non-template are compile errors. Only the lowered shapes
(`let` / call statements, `if` / `else`) are accepted; anything else
is a spanned `compile_error!`. Full details on the
[`#[workflow]`](workflow.md) page.

## 3. `#[container]` runs real Rust in a pod

A container body really does execute inside its pod. Arguments come
in as inputs; the return value goes out as the step's output. I/O
is `serde`-bound at compile time, so take and return owned types
(`String`, not `&str`).

Arguments can also be **spliced into the pod spec**: writing
`image = "repo:" + tag` injects an argument into the image (and
likewise into `service_account`, `node_selector`, `env`, and the
mutex `name` / `namespace`). See
[`#[container]` Parameter injection](container.md#parameter-injection).

## 4. Data between steps is JSON

Every value passed between steps is JSON-encoded into a parameter
and decoded by the receiver. This keeps a `String` `"7"` a string on
the wire (never silently parsed as a number).

It also makes more specialised plumbing fall out cleanly: `b.field`
on a binding wires only that field to the next task, and
`list.fan_out(|x| step(x))` runs `step` once per element of the list.

## 5. `#[fragment]` carries pod resources

Pod resources (`host!` mounts, S3 artifact ports, `secret!` envs)
are declared inside a `#[container]` or `#[fragment]`. A fragment is
a normal helper that runs as real code in the calling pod; every
resource it declares is added to each container that transitively
calls it. Share pod resources without a registry.

## 6. Your workflow binary runs in two worlds

The binary your workflow crate compiles to plays two roles:

- **On your machine**, `cargo athena emit` / `publish` / `submit`
  walks the template closure from your entrypoint and prints one
  `WorkflowTemplate` per template.
- **In Argo**, that same binary deserializes the step's inputs,
  calls the matching `#[container]` body, and serializes the return.

`cargo athena publish` cross-compiles it static-musl and uploads it;
`emit` adds it to every container template; a tiny `sh` bootstrap
picks the matching architecture and runs the right function. The
image needs only `sh` and `uname`.

---

With these in mind, the reference pages are the details:
[`#[workflow]`](workflow.md), [`#[container]`](container.md),
[the CLI](cli.md), and [`athena.toml`](configuration.md).
