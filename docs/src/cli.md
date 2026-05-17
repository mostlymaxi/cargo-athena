# The `cargo athena` CLI

After `cargo install cargo-athena` you have the `cargo athena`
subcommand. It drives *your* workflow crate's binary (the one whose
`main` calls `cargo_athena::entrypoint::<Root>()`) in the right mode.

```text
cargo athena [-c FILE] emit  [--package P] [--bin B] [--out FILE] [--with-workflow]
cargo athena [-c FILE] run   --template <argo-name> [--package P] [--bin B] [--input JSON]
cargo athena [-c FILE] build [--package P] [--bin B] [--target T].. [--print]
cargo athena            publish [--package P] [--bin B]            (not yet)
```

`-c, --config <FILE>` (global) points at an `athena.toml`. By default
the nearest one walking up from the cwd is used (like `Cargo.toml`), or
`$ATHENA_CONFIG`.

## `emit`

Relays the multi-document YAML: one `WorkflowTemplate` per reachable
template, cross-referenced by `templateRef`. The names are **stable and
deterministic** (`<crate>-<fn>`) — register them and trigger runs with
`argo submit --from workflowtemplate/<root>`.

```sh
cargo athena emit --package my-crate                    # to stdout
cargo athena emit --package my-crate --out wf.yaml      # to a file
cargo athena emit --package my-crate | kubectl apply -f -   # register
```

`--with-workflow` also appends a convenience runnable `Workflow`
(`generateName`, `workflowTemplateRef` → root), so
`cargo athena emit --with-workflow … | kubectl create -f -` registers
*and* fires one run — handy for demos. Off by default: a `generateName`
object isn't idempotent and isn't something you'd GitOps; the
deterministic templates are.

Needs only an [`athena.toml`](configuration.md) (it bakes the artifact
source into the YAML) — no cluster, S3, or cross-build. The fast
iteration loop.

## `run`

Executes one container's body locally, in-process, exactly as it would
run in-pod — great for unit-testing a single step's real logic without
a cluster:

```sh
cargo athena run --template my-crate-transform \
  --input '{"data":"hello","factor":4}'
```

- `--template` (required) is the Argo name — `<crate>-<fn>` kebab-case,
  or the `#[container(name = "…")]` override.
- `--input` is the JSON object of the function's arguments.

## `build`

Cross-compiles a **static-musl** binary for each target in
[`athena.toml`](configuration.md)'s matrix, packages them as
`app-<triple>` inside one `.tar.gz`, and prints the exact upload
destination:

```sh
cargo athena build --package my-crate           # build + package
cargo athena build --package my-crate --print   # dry run: just resolve + print the key
```

- `--target T` (repeatable) overrides the `athena.toml` target matrix.
- Requires the Zig cross toolchain: `cargo install cargo-zigbuild` and
  [`zig`](https://ziglang.org/download/). `build` checks for both up
  front and tells you exactly what to install if either is missing.

Upload the printed `.tar.gz` to the printed `s3://…` key with any S3
client (`s3cmd` / `aws s3 cp` / `mc cp`). `emit` injects that tarball
plus a tiny `sh` bootstrap into every container template, so one
artifact serves every step on any node architecture.

## `--package` / `--bin`

`cargo athena` runs *your* crate's binary; `--package` / `--bin` pick
which one in a multi-package or multi-binary workspace (same meaning as
for `cargo` itself). Omit them in a single-binary crate.

> Working in this repo instead of an installed binary? Any
> `cargo athena <cmd>` above is `cargo run -p cargo-athena --bin
> cargo-athena -- athena <cmd>`.
