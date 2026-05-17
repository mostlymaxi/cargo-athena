# Core Concepts

Five ideas explain everything cargo-athena does.

## 1. Templates are types

Each `#[workflow]` / `#[container]` lowers to a unit-struct **type** that
implements an internal `Template` trait. The name, inputs, and the
emitted YAML are derived by the compiler — so cross-crate, cross-module
references resolve through normal Rust name resolution and are
collision-proof.

Merely *referencing* a template type force-links its defining crate, and
emission walks the reachable closure from your entrypoint by direct,
monomorphic calls. There is **no registry, no `inventory` for
templates, no DCE concern** — nothing uncalled is emitted, and nothing
called is missed.

## 2. `#[workflow]` is a statically analyzed DAG

A workflow body is **read, not executed**. Each `let x = t(args);` /
`t(args);` becomes an Argo task; data flow between them becomes
`templateRef` wiring and DAG edges. Because it is analyzed, the body is
also type-checked as ordinary Rust by a hidden, never-run "ghost" copy —
so wrong types, arity, fields, or calling a non-template are compile
errors.

The body contract is **strict and fail-loud**: only the lowered shapes
(`let`/call statements and `if`/`else`) are accepted; anything else is a
spanned `compile_error!`, never a silent mis-emit. Full details on the
[`#[workflow]`](workflow.md) page.

## 3. `#[container]` runs real Rust in a pod

A container body genuinely executes. Arguments are Argo input
parameters, deserialized (serde) at pod start; the return value is
serialized to `outputs.parameters.return` for the next step. Container
I/O is compile-time bound to `serde` — take and return owned types.

Those same arguments can also be **spliced into the pod spec**:
`image = "repo:" + tag` injects an argument into the image (likewise
`service_account` and `node_selector` values). See
[`#[container]` → Parameter injection](container.md).

## 4. `#[fragment]` carries pod resources

Pod resources (`host!` mounts, S3 artifact ports) are declared with
macros that are only valid inside a container or a `#[fragment]`. A
fragment is a normal helper function: it runs as real code inside the
calling pod, and every resource it declares is collected onto each
container that transitively calls it. This makes resource declarations
composable without a global registry.

## 5. One binary, delivered through S3

There is a single multi-step binary. `cargo athena build`
cross-compiles it static-musl for each target in `athena.toml` and
packages them as one tarball in your S3 `ArtifactRepository`. `emit`
injects that tarball as an input artifact plus an `sh` bootstrap into
every container template; in-pod the bootstrap `uname`s, picks
`app-<triple>`, and execs it with `--cargo-athena-template <name>`.

All athena paths live under a pod-scoped `emptyDir` at `/athena`, so it
works on distroless / read-only-rootfs images — the image only needs a
POSIX `sh`, `tar`, and `uname`.

---

With these in mind, the reference pages are just the details:
[`#[workflow]`](workflow.md), [`#[container]`](container.md),
[the CLI](cli.md), and [`athena.toml`](configuration.md).
