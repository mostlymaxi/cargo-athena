# The `cargo athena` CLI

After `cargo install cargo-athena` you have the `cargo athena`
subcommand. The consumer commands (`emit`, `ls`, `describe`, `emulate`,
`submit`) act on a **workflow binary**: pass one as `[BINARY]` (a path,
or a name on `$PATH` from `cargo install`) and they need no source.
Omit it to build from the current crate instead (the developer loop), or
point at another crate with `--manifest-path`. `build` and `publish`
always build from source.

```text
cargo athena [-c F] init [PATH] [--name N] [--bucket B] [--endpoint E] [--region R] [-y]
cargo athena [-c F] doctor [--check-s3]
cargo athena [-c F] emit     [BINARY] [--out F] [--with-workflow]
cargo athena [-c F] ls       [BINARY] [--kind container|workflow] [--include-synthetic]
cargo athena [-c F] describe [BINARY] [-w TEMPLATE] [--json]
cargo athena [-c F] emulate  [BINARY] [-w TEMPLATE] [-a k=v].. [--input-file F]
                             [--build|--tarball F] [--runtime R] [--skip-artifacts]
cargo athena [-c F] submit   [BINARY] [-w TEMPLATE] [-a k=v].. [-n NS] [--service-account SA]
                             [--node-selector k=v].. [--priority N] [--argo-server URL] [-y] [--update]
cargo athena [-c F] build   [-p PKG] [--bin B] [--target T].. [--allow-dirty] [--dev-tag T] [-y] [--print]
cargo athena [-c F] publish [-p PKG] [--bin B] [--target T].. [--tarball F] [--allow-dirty] [--dev-tag T] [-y] [--print]
cargo athena [-c F] prune    <TAG> [BINARY] [--keep-binary] [-n NS] [--argo-server URL] [-y]

  [BINARY]   a cargo-athena binary (path or $PATH name). Omit to build from
             source instead: --manifest-path DIR / -p PKG / --bin B (default: the cwd crate).
```

The typical flow is **`publish`** to ship the binary, then
**`submit`** to register the templates and start a run. Use
[`init`](#init) to scaffold a fresh crate and [`doctor`](#doctor) to
check that your toolchain is ready.

`-c, --config <FILE>` (global) points at an `athena.toml`. With no flag
it is discovered automatically (`$ATHENA_CONFIG`, the nearest
`athena.toml` walking up from the cwd, then a global one), so `submit` /
`emit` / `ls` / `describe` work against an already-published workflow
with no per-repo config. See [`athena.toml`](configuration.md) for the
full precedence order.

## Versioning

Every emitted `WorkflowTemplate` carries a **version tag** in its name
(`<crate>-<fn>-<tag>`), so a cluster can hold many versions of the same
template-set side by side. The tag is **sealed into the binary at build
time** by `cargo athena build` / `publish`: it is read back, never
recomputed, so editing `Cargo.toml` after a build cannot change a built
binary's deployed version. A binary built with plain `cargo build` (no
wrapper) falls back to its `CARGO_PKG_VERSION` as a release tag.

There are two channels, gated like `cargo publish`:

- **release** - a clean tree on `main` (or `master`) → the tag is your
  crate's semver, kebab'd (`0.6.0` → `myapp-train-0-6-0`). This is the
  only way to mint a clean semver name.
- **dev** - anything else. Two **separate** gates decide it:
  - **`--allow-dirty`** - a dirty working tree would bake uncommitted
    code into the binary, so `build` / `publish` hard-fail without it.
  - **off a release branch** - a warning + confirmation (`-y` / `--yes`
    to skip, for CI).

  `--dev-tag` names the dev slot: bare `--dev-tag` uses the short commit
  (`myapp-train-dev-a1b2c3d`, a new slot per commit), while `--dev-tag
  foo` gives a **stable** slot (`myapp-train-dev-foo`) you overwrite
  while iterating. It forces the dev channel even on a clean release
  branch.

Each version's binary lands at its own S3 key
(`{pkg}/<tag>/{bin}.tar.gz`), so a dev build never clobbers a release.
Provenance rides in labels on every WT - `cargo.athena/tag`,
`cargo.athena/channel`, `cargo.athena/commit`, `cargo.athena/dirty` - so
`kubectl get wt -l cargo.athena/tag=<tag>` finds a version and
[`prune`](#prune) removes it.

**Fast-iteration loop.** `build`/`publish` resolve the tag from git, and a
source-build `submit`/`emit` (no positional `[BINARY]`) resolves it the
*same* way - so on a dev tree they agree on `dev-<commit>` with no setup:

```bash
cargo athena publish --allow-dirty   # -> ...-dev-<commit>, uploads to .../dev-<commit>/
cargo athena submit                  # same dev-<commit>; deploys + pulls that binary
# ... iterate: edit, re-run publish + submit
cargo athena prune dev-<commit>      # remove that version's WTs + S3 binary
```

The slot is the short commit, so it rolls each commit. For ONE **stable**
slot you overwrite in place (and so `submit` doesn't have to be a source
build), pin it with `ATHENA_VERSION_TAG` - it's respected by every step
(`build`/`publish`/`submit`/`emit`) and by a prebuilt binary:

```bash
export ATHENA_VERSION_TAG=dev-wip
cargo athena publish --allow-dirty   # -> ...-dev-wip
cargo athena submit                  # -> dev-wip
cargo athena prune dev-wip
```

`--dev-tag wip` on `build`/`publish` is the one-shot equivalent for a
single command. `ATHENA_VERSION_TAG` is also the **CI / cross-machine**
escape hatch: it forces the tag verbatim and skips git, so a build job
and a separate publish job (or a `publish --tarball`) agree on the key.

Note: `submit` follows the **binary's** baked tag. A source build bakes
the git-aware tag automatically (above); but if you hand `submit` a
prebuilt `[BINARY]`, it uses *that* binary's tag - so don't submit a
plain `cargo build` artifact (tagged as a release) expecting a dev tag.

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
cargo athena emit ./my-workflow                  # a built or installed binary
cargo athena emit ./my-workflow --out wf.yaml
cargo athena emit --package my-crate | kubectl apply -f -   # build from source
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
cargo athena submit ./my-workflow -w pipeline -a seed=hello
W=$(cargo athena submit ./my-workflow -w pipeline -a seed=hello -y)   # scriptable
cargo athena submit ./my-workflow -a seed=hello   # -w omitted: the binary's root
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
- `-w TEMPLATE` / `--workflow` - which template to submit (default: the
  binary's root). `<crate>-<fn>` kebab or the short `<fn>` form.
- `-a name=value` (repeatable) / `--input-file F` - workflow arguments.
- `-n NS` / `--namespace` - target namespace.
- `--service-account SA` - override `[defaults].service_account`.
- `--node-selector k=v` (repeatable) - root-scoped, applies to every pod.
- `--priority N` - workflow priority (int32); higher = scheduled first
  when the controller hits its parallelism limit.
- `--argo-server URL` - use Argo Server REST instead of Kubernetes API.
- `--insecure-skip-tls-verify` - skip TLS verification talking to the
  Argo Server.
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
- `--allow-dirty` / `--dev-tag [T]` / `-y` - the version gates (see
  [Versioning](#versioning)). They resolve the tag baked into the binary
  and the S3 key.
- `--print` - dry run: resolve and print the destination key (and the
  resolved tag), no build or upload.
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

Same flags as `publish` minus `--tarball` and the upload step, including
the `--allow-dirty` / `--dev-tag` version gates.

## `prune`

Remove one deployed [version](#versioning) of this binary's
template-set: every `WorkflowTemplate` labelled `cargo.athena/tag=<TAG>`,
plus its `{pkg}/<tag>/{bin}.tar.gz` S3 binary. For cleaning up dev
iterations.

```sh
cargo athena prune dev-wip ./app        # delete the dev-wip WTs + its S3 binary
cargo athena prune 0.5.0 ./app --keep-binary   # WTs only; leave the tarball
```

`<TAG>` is a dev slot (`dev-wip`), a release tag (`0-6-0`), or a raw
semver (`0.6.0`, normalized). `pkg` / `bin` come from the probed binary,
and the selector always pins both package and tag, so it can never
fan out into a broad delete. Prints what it will remove and asks for
confirmation (`-y` to skip). Talks to the same transport as `submit`
(`--argo-server` / `$ARGO_SERVER`, else the kube API).

## `emulate`

Runs one `#[container]` locally under docker or podman. `-w` picks the
container (default: the binary's root template).

```sh
cargo athena emulate ./my-workflow -w transform -a data=hello -a factor=4
cargo athena emulate ./my-workflow -w fetch --input-file args.json
cargo athena emulate --build -w fetch        # build from source for the run
```

The metadata comes from the `[BINARY]` you name; the run payload is the
deployed tarball pulled from S3 by default, so you smoke-test what's
actually live. Arguments are type-checked against the real function
signature; missing or wrong-type values fail fast.

**Not emulated:** anything Kubernetes-specific. `docker run` has no
ServiceAccount, no RBAC, no `nodeSelector`. For those, use `submit`
on a real cluster.

Flags:
- `-w TEMPLATE` / `--workflow` - the container to emulate (default: root).
- `-a name=value` (repeatable) / `--input-file F` - function arguments.
- `--build` - build a fresh local host-arch musl binary for the run
  instead of pulling the deployed tarball (source-only; omit `[BINARY]`).
- `--tarball F` - use `F` verbatim.
- `--runtime docker|podman` - autodetect by default (prefer docker).
- `--skip-artifacts` - bypass S3 `load`/`save_artifact!` sync.

## `describe`

Shows what one template expects: its signature, image, mounts, and a
copy-pasteable `submit` line. Works for either a `#[container]` or a
`#[workflow]`.

```sh
cargo athena describe ./my-workflow              # the root template
cargo athena describe ./my-workflow -w transform
cargo athena describe ./my-workflow --json       # raw metadata, for scripts
```

## `ls`

Lists the templates your workflow binary exposes.

```sh
cargo athena ls ./my-workflow                       # every template
cargo athena ls ./my-workflow --kind container      # #[container]s only
cargo athena ls ./my-workflow --kind workflow       # #[workflow]s only
cargo athena ls ./my-workflow --include-synthetic   # + if/else internals
```

## Selecting the binary

The consumer commands (`emit`, `ls`, `describe`, `emulate`, `submit`)
resolve their program in this order:

1. the `[BINARY]` positional - a path, or a bare name on `$PATH` (e.g.
   one installed with `cargo install`). No source needed.
2. otherwise a **source build**: `--manifest-path DIR` (or the current
   crate), narrowed by `-p` / `--package` and `--bin` (which fall back to
   `[defaults].package` / `.bin` in `athena.toml`).

`-w` / `--workflow` names the template to act on and defaults to the
binary's root (`entrypoint!(Root)`). `build` and `publish` always build
from source and take `-p` / `--bin` (never `[BINARY]`).

> Working in this repo instead of an installed binary? Any
> `cargo athena <cmd>` above becomes
> `cargo run -p cargo-athena --bin cargo-athena -- athena <cmd>`.
