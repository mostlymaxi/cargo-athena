# cargo-athena-cli

[![crates.io](https://img.shields.io/crates/v/cargo-athena-cli.svg)](https://crates.io/crates/cargo-athena-cli)

The `cargo athena` subcommand for
[`cargo-athena`](https://crates.io/crates/cargo-athena) — emit Argo
`WorkflowTemplate` YAML, cross-compile + upload the binary, and run a
template locally.

```sh
cargo install cargo-athena-cli

cargo athena emit  --package my-workflows
cargo athena build --package my-workflows
cargo athena run   --package my-workflows --template my-crate-my-fn --input '{"a":"hi"}'
```

Write your workflows against
**[`cargo-athena`](https://crates.io/crates/cargo-athena)**.

- Repository: <https://github.com/mostlymaxi/cargo-athena>
- License: MIT OR Apache-2.0
