# cargo-athena-macros

[![crates.io](https://img.shields.io/crates/v/cargo-athena-macros.svg)](https://crates.io/crates/cargo-athena-macros)
[![docs.rs](https://img.shields.io/docsrs/cargo-athena-macros)](https://docs.rs/cargo-athena-macros)

The `#[workflow]`, `#[container]`, and `#[fragment]` proc macros behind
[`cargo-athena`](https://crates.io/crates/cargo-athena).

This is an internal crate. Depend on
**[`cargo-athena`](https://crates.io/crates/cargo-athena)** instead: it
re-exports these behind the `::cargo_athena` path the generated code
targets. The complete feature reference for each macro is on
[docs.rs](https://docs.rs/cargo-athena-macros) (the `WORKFLOW.md` /
`CONTAINER.md` content is rendered directly on the `workflow` /
`container` macros).

- Repository: <https://github.com/mostlymaxi/cargo-athena>
- License: MIT OR Apache-2.0
