# Examples

Six examples live in the [repo's `examples/`](https://github.com/mostlymaxi/cargo-athena/tree/main/examples)
directory, each illustrating something different.

## `getting-started`

The smallest end-to-end pipeline: three containers in a `fetch ->
summarize -> publish` chain. Mirrors the
[Getting Started](getting-started.md) walkthrough on the docs site.

[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/getting-started/src/main.rs)
· [Emitted YAML](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/getting-started/emit.yaml)

## `basic`

The minimum viable shape: two `#[workflow]` functions, one
`#[container]`, one `#[fragment]`. Good for seeing the macros without
surrounding detail.

[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/basic/src/main.rs)

## `smoke`

The "all features" fixture used by the project's own goldens.
Exercises every macro attribute (retry, timeouts, mutexes,
nodeSelector, on_exit, hooks), the conditional shapes
(value-if, statement-if/else-if/else), `fan_out` over a list, named
struct-field forwarding (`a.field`), `host!` mounts, S3 artifact ports,
`secret!`, fragments, nested calls, and `steps` mode.

If you want one place to see a pattern in action, this is it.

[Library source](https://github.com/mostlymaxi/cargo-athena/tree/main/examples/smoke/src)
· [Golden YAML per feature](https://github.com/mostlymaxi/cargo-athena/tree/main/examples/smoke/tests/golden)

## `importing`

Cross-module and cross-crate composition: this crate depends on
`smoke` and references one of its templates from a new `#[workflow]`
in another crate. Demonstrates that templates compose through normal
Rust name resolution.

[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/importing/src/main.rs)

## `e2e`

The crate the project's GitHub Actions matrix actually submits to a
real Argo + MinIO on every push to `main`. It's the live conformance
test for every supported Argo version (4.0.8 / 3.7.17 / 3.6.19).

If you want to see a workflow that's verified-running on real Argo,
this is the one.

[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/e2e/src/main.rs)

## tracing

A two-container `greet -> shout` pipeline that emits `tracing::info!`
records. The subscriber is installed once in `main()`, gated on
`cargo_athena::is_container_run()` so it fires only in-pod, never on a
local `cargo athena emit` / `ls` / `submit`. The runnable companion to
the cookbook's
[Set up tracing](cookbook.md#set-up-tracing-or-any-pod-only-init-once-for-every-container)
recipe.

[Source](https://github.com/mostlymaxi/cargo-athena/blob/main/examples/tracing/src/main.rs)
