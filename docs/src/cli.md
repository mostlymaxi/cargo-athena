# The `cargo athena` CLI

After `cargo install cargo-athena` you have the `cargo athena`
subcommand. It drives *your* workflow crate's binary (the one whose
`main` calls `cargo_athena::entrypoint::<Root>()`) in the right mode.

```text
cargo athena [-c F] emit  [-p PKG] [--bin B] [--out F] [--with-workflow]
cargo athena [-c F] container ls       [-p PKG] [--bin B] [--all]
cargo athena [-c F] container emulate  <name> [-a k=v].. [--input-file F] [-p PKG] [--bin B]
                                       [--build|--tarball F] [--runtime R] [--skip-artifacts]
cargo athena [-c F] container describe <name> [-p PKG] [--bin B]
cargo athena [-c F] workflow  ls       [-p PKG] [--bin B] [--include-synthetic]
cargo athena [-c F] workflow  describe <name> [-p PKG] [--bin B]
cargo athena [-c F] submit <name> [-a k=v].. [-n NS] [--service-account SA]
                          [--node-selector k=v].. [--argo-server URL] [-y] [--update]
cargo athena [-c F] build [-p PKG] [--bin B] [--target T].. [--print]
cargo athena [-c F] publish [-p PKG] [--bin B] [--target T].. [--print]
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

## `container emulate`

Runs one `#[container]` locally under **docker/podman, exactly as Argo
would**: the same image, the *same injected bootstrap*, the same
`ATHENA_PARAM_*` env, the `/athena` scratch dir, `host!` binds, and S3
artifact ports. Test a single node locally — no Kubernetes, no source
on the node.

```sh
# default: pull the *deployed* binary from S3 and run it in its image
cargo athena container emulate my-crate-transform -a data=hello -a factor=4

cargo athena container emulate my-crate-fetch --input-file args.json
cargo athena container emulate my-crate-fetch --build         # local musl build instead
```

Fidelity is by construction: the binary reports its run metadata from
the *same* `Template::build()` `emit` uses, so there's nothing to keep
in sync.

- `<name>` (positional) — the full template name (`<crate>-<fn>` kebab,
  or the `#[container(name = "…")]` override). `cargo athena container
  ls` lists them. A `#[workflow]` is rejected (it's a DAG, not a pod —
  emulate its containers individually).
- `-a name=value` (repeatable, **`--arg`**) / `--input-file F` — the
  function arguments. A value is parsed as JSON if it parses (`-a n=4` →
  number), else a string; all are JSON-encoded into the env exactly as
  Argo passes them. Arguments are **type-checked against the fn's real
  signature before anything launches** — missing, unknown (with
  did-you-mean), and wrong scalar/array kinds fail fast.
- `-p`/`--package`, `--bin` select the cargo target (see
  [package selection](#package-selection)).
- Binary source: **default = pull the deployed tarball from the
  `athena.toml` S3 repo** (smoke-test what's live). `--build` packages a
  local host-arch musl binary; `--tarball F` uses one verbatim. S3
  credentials come from the standard `AWS_*` env vars.
- `--runtime docker|podman` (default: autodetect, prefer docker);
  `--skip-artifacts` to bypass S3 `load/save_artifact!` sync.

**Limitations — this runs the container *body* faithfully, not the
pod's Kubernetes context.** `docker run` has no notion of a
`ServiceAccount`, so `#[container(service_account=…)]` and any
podSpec-level concerns (RBAC, `nodeSelector`, podSpecPatch) are **not**
emulated. For those, exercise the real Argo path (`emit` + submit).

## `container describe`

Prints, as JSON, the exact runner metadata one template reports — its
image, parameters **and their Rust types**, the binary/`host!`/artifact
S3 ports, and the scratch + result paths. It's *the same* metadata
`emulate` consumes (derived from the same `Template::build()` as
`emit`), so it's the way to see what *would* run, or to script around
it:

```sh
cargo athena container describe my-crate-transform
```

## `container ls`

Lists the templates your workflow binary reports — full name, kind, and
typed args — so they're discoverable for `emulate`/`describe` (no
guessing the `<crate>-<fn>` name):

```sh
cargo athena container ls            # #[container]s only
cargo athena container ls --all      # + #[workflow]s and synthetic templates
```

```text
NAME                                  KIND       ARGS
my-crate-fetch                        container  url: String
my-crate-transform                    container  data: String, factor: i64
```

## `workflow ls`

The `#[workflow]`s in the package (name + typed inputs). athena's
synthesized `if`/`else` wrapper + arm sub-workflows are an
implementation detail, so they're **hidden unless
`--include-synthetic`**:

```sh
cargo athena workflow ls                      # your #[workflow]s
cargo athena workflow ls --include-synthetic  # + the if/else machinery
```

## `workflow describe`

Same metadata dump as [`container describe`](#container-describe), for
any template — handy on a `#[workflow]` to see its resolved inputs:

```sh
cargo athena workflow describe my-crate-pipeline
```

## `submit`

Run a `#[workflow]` (or a single `#[container]`) on a real cluster —
`argo submit --from workflowtemplate/<name>` with the safety rails you'd
otherwise do by hand:

```sh
cargo athena submit my-crate-pipeline -a seed=hello
W=$(cargo athena submit my-crate-pipeline -a seed=hello -y)   # scriptable
```

Before anything is created it:

1. **type-checks** `-a`/`--input-file` against the template's real
   signature (same report as `emulate`);
2. confirms the **binary tarball is uploaded** (so pods can bootstrap;
   `--skip-binary-check` to bypass);
3. **registers + drift-checks** every reachable `WorkflowTemplate`:
   missing ones are *created*, ones that differ from `emit` are
   *updated* — after a **y/N prompt** (the change list is shown;
   `--update` re-applies all, `-y/--yes` skips every prompt);
4. creates the `Workflow` (a second **y/N**), then prints its **name to
   stdout** (everything else is on stderr, so `W=$(… -y)` works).

Transport mirrors the `argo` CLI: with **`--argo-server`/`$ARGO_SERVER`**
set it uses the **Argo Server REST API** (`$ARGO_TOKEN` for auth,
`--insecure-skip-tls-verify` if needed); otherwise it creates the CR
through the **Kubernetes API** via your kubeconfig / in-cluster config
(client-certs, tokens, and EKS/GKE/AKS exec-credential plugins all work
— it's `kube-rs`).

Overrides: `-n/--namespace` (`$ARGO_NAMESPACE` →
[`[defaults].namespace`](configuration.md) → `default`),
`--service-account` (→ `[defaults].service_account`), and
`--node-selector k=v` (repeatable; set root-scoped on the submitted
Workflow — Argo applies it to every pod).

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

## `cargo athena publish`

The one-shot **build + upload**: cross-compiles + packages (exactly
like `build`) and then uploads the tarball to
[`athena.toml`](configuration.md)'s artifact repository — the same key
`emit` resolves, so it lands where the injected bootstrap fetches it:

```sh
cargo athena publish --package my-crate   # cross-compile + package + upload
```

- Takes the same flags as `build` (`-p/--bin`, `--target`, `--print`).
  `--print` is a dry run (resolve + print the key, no build/upload).
  Use plain `build` when you want the tarball locally *without*
  uploading (CI artifact, inspection).
- S3 credentials come from the standard `AWS_ACCESS_KEY_ID` /
  `AWS_SECRET_ACCESS_KEY` (/ `AWS_SESSION_TOKEN`) environment variables
  — the same `object_store` path `submit`/`emulate` use.
- The destination `s3://bucket/key` is printed on **stdout**
  (scriptable); progress on stderr.

`emit` injects that tarball plus a tiny `sh` bootstrap into every
container template, so one artifact serves every step on any node
architecture.

## Package selection

`cargo athena` runs *your* crate's binary. Which one is resolved, in
order:

1. **`-p`/`--package` and `--bin`** flags (same meaning as for `cargo`
   itself);
2. else **`[defaults]` in `athena.toml`** — `package = "…"` /
   `bin = "…"` (set them once instead of repeating the flags, like a
   project default);
3. else cargo's single-package / default-bin autodetect.

So in a configured workspace `cargo athena container ls` and
`cargo athena container emulate my-crate-fetch -a url=…` just work with
no target flags. (`-p` is **package** here — function arguments to
`emulate` are `-a`/`--arg`.)

> Working in this repo instead of an installed binary? Any
> `cargo athena <cmd>` above is `cargo run -p cargo-athena --bin
> cargo-athena -- athena <cmd>`.
