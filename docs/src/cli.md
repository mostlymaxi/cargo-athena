# The `cargo athena` CLI

`cargo athena` drives a user crate's cargo-athena binary. It has three
working subcommands (and a `publish` stub):

```text
cargo athena emit    [--package P] [--bin B] [--out FILE]
cargo athena run     --template <argo-name> [--package P] [--bin B] [--input JSON]
cargo athena build   [--package P] [--bin B] [--target T].. [--print]
cargo athena publish [--package P] [--bin B]            (not yet)
```

The entrypoint is fixed in the user binary's `main`
(`cargo_athena::entrypoint::<Root>()`); the CLI just runs that binary in
the right mode. In this workspace, invoke it via
`cargo run -q -p cargo-athena --bin cargo-athena -- athena <subcommand>`.

## `emit`

Runs the user binary in emit-mode and relays the multi-document YAML:
one `WorkflowTemplate` per reachable template, cross-referenced by
`templateRef`. These have **stable, deterministic names**
(`<crate>-<fn>`) — register them and trigger runs with
`argo submit --from workflowtemplate/<root>`.

```sh
cargo athena emit --package my-crate                    # to stdout
cargo athena emit --package my-crate --out wf.yaml      # to a file
cargo athena emit --package my-crate | kubectl apply -f -   # register
```

`--with-workflow` additionally appends a convenience runnable
`Workflow` (`generateName`, `workflowTemplateRef` → root) so
`cargo athena emit --with-workflow … | kubectl create -f -` registers
*and* fires one run — handy for demos. It's off by default because a
`generateName` object isn't idempotent and isn't something you'd
GitOps; the deterministic templates are.

Needs an [`athena.toml`](configuration.md) (it bakes the artifact
source into the YAML); no cluster, S3, or cross-build — the fast
iteration loop.

## `run`

Executes one container's body locally, in-process, exactly as it would
run in-pod: it sets the template + input and runs the user binary in
run-mode.

```sh
cargo run -q -p cargo-athena --bin cargo-athena -- athena run \
  --template my-crate-transform \
  --package my-crate \
  --input '{"data":"hello","factor":4}'
```

- `--template` (required) is the Argo name — `<crate>-<fn>` kebab-case,
  or the `#[container(name = "…")]` override.
- `--input` is the JSON object of the function's arguments.

Great for unit-testing a single step's real logic without a cluster.

## `build`

Cross-compiles a **static-musl** binary for each target in
[`athena.toml`](configuration.md)'s matrix, packages them as
`app-<triple>` inside one `.tar.gz`, and reports the upload key. Uses
`cargo-zigbuild` + `zig` (in the dev shell).

```sh
cargo run -q -p cargo-athena --bin cargo-athena -- athena build --package my-crate --print
```

- `--target T` (repeatable) overrides the `athena.toml` target matrix.
- `--print` does a dry run (resolve + report the key) without uploading
  — used by CI.

`emit` injects this tarball plus the `sh` bootstrap into every container
template, so a single artifact serves every step on any node
architecture.

## `--package` / `--bin`

Both are passed straight through to `cargo run`, so `cargo athena`
targets the same binary `cargo` would. Omit them in a single-binary
crate.
