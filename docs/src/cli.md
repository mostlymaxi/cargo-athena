# The `cargo athena` CLI

After `cargo install cargo-athena` you have the `cargo athena`
subcommand. It drives your workflow crate's binary.

```text
cargo athena [-c F] init [PATH] [--name N] [--bucket B] [--endpoint E] [--region R] [-y]
cargo athena [-c F] doctor [--check-s3]
cargo athena [-c F] emit  [-p PKG] [--bin B] [--out F] [--with-workflow]
cargo athena [-c F] container ls       [-p PKG] [--bin B] [--all]
cargo athena [-c F] container emulate  <name> [-a k=v].. [--input-file F] [-p PKG] [--bin B]
                                       [--build|--tarball F] [--runtime R] [--skip-artifacts]
cargo athena [-c F] container describe <name> [-p PKG] [--bin B]
cargo athena [-c F] workflow  ls       [-p PKG] [--bin B] [--include-synthetic]
cargo athena [-c F] workflow  describe <name> [-p PKG] [--bin B]
cargo athena [-c F] submit <name> [-a k=v].. [-n NS] [--service-account SA]
                          [--node-selector k=v].. [--priority N]
                          [--argo-server URL] [-y] [--update]
cargo athena [-c F] build [-p PKG] [--bin B] [--target T].. [--print]
cargo athena [-c F] publish [-p PKG] [--bin B] [--target T].. [--tarball F] [--print]
```

The typical flow is **`publish`** to ship the binary, then
**`submit`** to register the templates and start a run. Use
[`init`](#init) to scaffold a fresh crate and [`doctor`](#doctor) to
check that your toolchain is ready.

`-c, --config <FILE>` (global) points at an `athena.toml`. By default
the nearest one walking up from the cwd is used (like `Cargo.toml`),
or `$ATHENA_CONFIG`.

## `init`

Scaffold a new workflow crate: writes a minimal `Cargo.toml`,
`src/main.rs`, and `athena.toml` in the target directory.

```sh
cargo athena init my-pipeline           # interactive (prompts for bucket/endpoint/region)
cargo athena init my-pipeline -y        # accept defaults, no prompts
cargo athena init -y --bucket my-bucket --region eu-west-1 .
```

Refuses to overwrite an existing `Cargo.toml`. For adding cargo-athena
to an existing crate, just run
`cargo add cargo-athena --no-default-features`.

Flags:
- `--name N` - cargo package name (default: directory basename).
- `--bucket` / `--endpoint` / `--region` - prefill `athena.toml`.
- `-y` / `--yes` - skip the interactive prompts.

## `doctor`

Preflight every prereq for `publish` and `submit`. Reports each as
green / red with a fix hint when something is missing:

```sh
cargo athena doctor
cargo athena doctor --check-s3   # also try a live HEAD on the bucket
```

Checks: `athena.toml` parses, `cargo-zigbuild` and `zig` are
installed, the rustup targets in `athena.toml [bootstrap].targets`
are present, and `AWS_*` env credentials are set (warning, not
fatal, since IMDS / IRSA cover the ambient case). With `--check-s3`,
also confirms the configured bucket actually responds.

Exit code is 0 on all-pass, 1 if anything failed.

## `emit`

Prints the multi-document `WorkflowTemplate` YAML to stdout.

```sh
cargo athena emit --package my-crate
cargo athena emit --package my-crate --out wf.yaml
cargo athena emit --package my-crate | kubectl apply -f -
```

Names are deterministic (`<crate>-<fn>` kebab) so the output is
GitOps-friendly. For the typical deploy + run flow use
[`publish`](#publish) and [`submit`](#submit) instead.

Flags:
- `--out F` - write to a file instead of stdout.
- `--with-workflow` - also append a runnable `Workflow` so
  `kubectl create -f -` registers AND fires one run (handy for demos).

## `submit`

Run a `#[workflow]` (or a single `#[container]`) on a real cluster.

```sh
cargo athena submit my-crate-pipeline -a seed=hello
W=$(cargo athena submit my-crate-pipeline -a seed=hello -y)   # scriptable
```

Before anything is created, `submit`:

1. type-checks the arguments against the function signature,
2. confirms the binary tarball is uploaded,
3. registers every WorkflowTemplate (asking y/N if any drifted),
4. creates the Workflow and prints its name to stdout.

Transport auto-selects: with `--argo-server` / `$ARGO_SERVER` set it
uses the Argo Server REST API (`$ARGO_TOKEN` for auth); otherwise it
uses your kubeconfig (EKS / GKE / AKS exec plugins all work).

Flags:
- `-a name=value` (repeatable) / `--input-file F` - workflow arguments.
- `-n NS` / `--namespace` - target namespace.
- `--service-account SA` - override `[defaults].service_account`.
- `--node-selector k=v` (repeatable) - root-scoped, applies to every pod.
- `--priority N` - workflow priority (int32); higher = scheduled first
  when the controller hits its parallelism limit.
- `--argo-server URL` - use Argo Server REST instead of Kubernetes API.
- `-y` / `--yes` - skip every y/N prompt.
- `--update` - re-apply all WorkflowTemplates.
- `--skip-binary-check` - don't verify the tarball is uploaded.

## `publish`

Cross-compiles a static-musl binary, packages it as a `.tar.gz`, and
uploads it to the artifact repository in `athena.toml`.

```sh
cargo athena publish --package my-crate
```

Requires the Zig cross toolchain: `cargo install cargo-zigbuild` and
[`zig`](https://ziglang.org/download/). `publish` checks for both up
front and tells you what's missing.

S3 credentials come from `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY`
(plus `AWS_SESSION_TOKEN` if you use it), or from instance-role
identity (EC2 IMDS / ECS task role / IRSA). The shared
`~/.aws/credentials` file is **not** read.

Flags:
- `--target T` (repeatable) - override the `athena.toml` target matrix.
- `--tarball F` - upload `F` verbatim; skip the build (build-once / upload-many).
- `--print` - dry run: resolve and print the destination key, no build or upload.
- `AWS_ENDPOINT_URL` env var - override the endpoint for this upload
  only (port-forward / public-vs-in-cluster split).

## `build`

The package-only variant of `publish`. Cross-compiles and writes the
`.tar.gz` locally without uploading - useful for CI artifacts or
inspection.

```sh
cargo athena build --package my-crate
cargo athena build --package my-crate --print    # just resolve + print the key
```

Same flags as `publish` minus `--tarball` and the upload step.

## `container emulate`

Runs one `#[container]` locally under docker or podman.

```sh
cargo athena container emulate my-crate-transform -a data=hello -a factor=4
cargo athena container emulate my-crate-fetch --input-file args.json
cargo athena container emulate my-crate-fetch --build
```

By default it pulls the deployed tarball from S3 so you smoke-test
what's actually live. Arguments are type-checked against the real
function signature; missing or wrong-type values fail fast.

**Not emulated:** anything Kubernetes-specific. `docker run` has no
ServiceAccount, no RBAC, no `nodeSelector`. For those, use `submit`
on a real cluster.

Flags:
- `-a name=value` (repeatable) / `--input-file F` - function arguments.
- `--build` - use a fresh local host-arch musl build instead of the
  deployed tarball.
- `--tarball F` - use `F` verbatim.
- `--runtime docker|podman` - autodetect by default (prefer docker).
- `--skip-artifacts` - bypass S3 `load`/`save_artifact!` sync.

## `container describe` / `workflow describe`

Prints one template's runner metadata as JSON: image, parameters
and their Rust types, S3 ports, scratch paths. Used by `emulate`
and scriptable.

```sh
cargo athena container describe my-crate-transform
cargo athena workflow  describe my-crate-pipeline
```

## `container ls` / `workflow ls`

Lists the templates your workflow binary reports.

```sh
cargo athena container ls            # #[container]s only
cargo athena container ls --all      # + #[workflow]s and synthetic templates

cargo athena workflow ls
cargo athena workflow ls --include-synthetic   # + the if/else machinery
```

athena's synthesized `if` / `else` wrapper sub-workflows are an
implementation detail and hidden unless you ask for them.

## Package selection

Which workflow binary `cargo athena` drives, in order:

1. `-p` / `--package` and `--bin` flags (same meaning as `cargo`).
2. `[defaults].package` / `.bin` in `athena.toml`.
3. cargo's single-package / default-bin autodetect.

So in a configured workspace, no target flags are needed on any
command. `container emulate` and `submit` use `-a` / `--arg` for
function arguments (since `-p` is already package).

> Working in this repo instead of an installed binary? Any
> `cargo athena <cmd>` above becomes
> `cargo run -p cargo-athena --bin cargo-athena -- athena <cmd>`.
