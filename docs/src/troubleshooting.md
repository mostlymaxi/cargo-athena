# Troubleshooting

Common errors and what they actually mean.

## Compile errors

### "expected type, found function"

```
error[E0573]: expected type, found function `my_fragment`
```

You called a `#[fragment]` or a regular Rust function from a
`#[workflow]` body. Workflow bodies only accept template calls
(`#[workflow]` or `#[container]`), nothing else. Either:

- Move the helper into a `#[container]` body (where regular code is
  allowed), or
- Convert the helper to a `#[fragment]` and call it from a
  `#[container]` (fragments run inside the calling pod).

### "unsupported statement in a #[workflow]" / shape-specific variants

The macro emits a targeted message per shape with a hint at what *is*
supported:

- **`for`** -> *"For per-element parallel work use `list.fan_out(|x| step(x))`; for sequential work, thread a return value through."*
- **`while`/`loop`** -> *"A #[workflow] body is read once to build the DAG, not iterated at runtime. Move the loop inside a #[container] body, or use `.fan_out` for parallelism."*
- **`match`** -> *"For exclusive branches use `if`/`else if`/`else` (supported)."*
- **`.method()`** -> *"The lowered chains are `.clone()`/`.to_owned()` on args, `.fan_out`, `.continue_on`, `.on_success/.on_failure/.on_error/.on_exit/.hook_if`."*
- **macros** -> *"A macro call here would be dropped from the DAG. If you need pod resources (`host!`, `secret!`, `load_artifact!`, `save_artifact!`), declare them inside a #[container] body."*

Supported shapes are:

```rust,ignore
let x = template(args);
template(args);
if cond { ... } else { ... }
binding.fan_out(|x| template(x, …));
binding.continue_on(...)       // and the other per-task hooks
```

### "the trait `Injectable` is not implemented for ..."

You used `image = "repo:" + arg` (or similar) where `arg` is a type
that doesn't round-trip through `serde_json` to a raw scalar. Only
`String`, `&str`, and the numeric primitives are injectable. Use a
literal, or change the argument's type.

### Argument name like `no` / `yes` / `on` / `true` rejected

```
error: argument name "no" would be reinterpreted by YAML 1.1 as a bool
```

Argo's YAML parser silently turns bare `y`/`yes`/`n`/`no`/`on`/`off`/
`true`/`false`/`null`/`~` (any case) into bools or null, which would
mis-type a parameter named that. Rename the argument.

## Runtime errors (from `cargo athena ...`)

### "binary tarball not found at s3://..."

You ran `cargo athena submit` but haven't run `cargo athena publish`
on the current version, or the upload was cleaned up. Either:

- Run `cargo athena publish` first, then re-submit, or
- Pass `--skip-binary-check` if you're sure (e.g. testing against a
  fixture upload).

The S3 object key embeds the package version
(`{crate}/{version}/{bin}.tar.gz`), so a `Cargo.toml` version bump
needs a fresh `publish`.

### "could not list templates" / "could not get template metadata"

```
cargo athena container ls: could not list templates
(run from your workflow crate, or pass --package/--bin)
```

`cargo athena` can't find a workflow binary to drive. Either:

- Run from inside your workflow crate, or
- Pass `-p <package>` (and `--bin <name>` if the crate has multiple bins), or
- Set `[defaults].package` (and optionally `.bin`) in `athena.toml`.

If you get a compile error from the user binary, that's now streamed
to your terminal: scroll up.

### S3 credentials / endpoint issues

cargo-athena reads the standard `AWS_ACCESS_KEY_ID`,
`AWS_SECRET_ACCESS_KEY`, and (if used) `AWS_SESSION_TOKEN` env vars,
plus the ambient cloud identity (EC2 IMDS, ECS task role, IRSA web
identity).

**Not read:** `~/.aws/credentials` and `AWS_PROFILE`. The S3 client is
`object_store`, not the AWS SDK, and the shared-config file is
unsupported. If you rely on a profile, export the credentials before
running:

```sh
eval "$(aws configure export-credentials --profile prod --format env)"
cargo athena publish
```

For a custom endpoint (MinIO, self-hosted), `athena.toml` sets the
endpoint pods use. To override it just for the upload from your
machine (e.g. port-forwarded MinIO from outside the cluster), set
`AWS_ENDPOINT_URL` for that one command. `emit` still bakes the
config's endpoint into the YAML, so pods always hit the in-cluster
address.

## In-cluster errors

### Workflow `Pending` forever, no events

If your Argo controller is configured `managedNamespace=<X>`, it
*only* watches that namespace. A Workflow submitted to the wrong
namespace stays Pending with no phase, no events, no errors.

Check the controller config:

```sh
kubectl -n argo get configmap workflow-controller-configmap -o yaml | grep namespace
```

Submit with `-n <X>` or set `[defaults].namespace` in `athena.toml`.

### Pods 403 on `workflowtaskresults`

```
pods "xxx" is forbidden: User "system:serviceaccount:..." cannot create
resource "workflowtaskresults" in API group "argoproj.io"
```

The workflow ServiceAccount needs the Argo executor role binding.
`namespace-install.yaml` omits it for the `default` SA, so a fresh
install in a namespace other than `argo` ends up with every step
failing 403.

Bind the executor role to the SA used by your pods (see the project's
`scripts/deploy.sh` for the kubectl invocation we use in CI).

### "configmaps is forbidden: ... cannot create resource configmaps"

```
task '...' errored: configmaps is forbidden: User
"system:serviceaccount:argo:argo" cannot create resource "configmaps"
in API group "" in the namespace "argo"
```

A task's substituted arguments crossed the 128 KB threshold and Argo
3.7+ tried to stage them in a per-pod `ConfigMap` (PR #15265). The
upstream `namespace-install.yaml` grants the controller SA only
read access on configmaps. Add `create` (and `update/patch/delete`
for resilience) to the controller's Role - `scripts/deploy.sh` does
this with `Role/RoleBinding athena-argo-configmaps`.

On Argo 3.6 the offload path doesn't exist at all; a substituted
`c.Args[]` over the kernel exec `ARG_MAX` (~128 KB) fails with
`exec /var/run/argo/argoexec: argument list too long` and the only
fix is using `Artifact<T>` for the large value.

### "failed to resolve {{tasks.X.outputs.*}}" on Argo ≤ 3.5

cargo-athena emits one `WorkflowTemplate` per template, wired via
`templateRef`. Argo 3.5 and older can't resolve cross-template task
output references at submit time, so any multi-step workflow fails
instantly with this message.

Fixed in Argo 3.6. The emitted YAML is correct and passes 3.6 / 3.7 /
4.0 unchanged. cargo-athena does not support Argo ≤ 3.5; do not try
to "fix" by inlining.

### Pod CrashLoopBackOff, "exec format error" or "no such file"

The injected bootstrap couldn't pick a matching binary for the node's
architecture. Check that the `targets` list in `athena.toml`
[`[bootstrap]`](configuration.md#bootstrap) includes a triple matching
each node where pods can land. The default
(`x86_64-unknown-linux-musl`, `aarch64-unknown-linux-musl`) covers
most clusters; if you only build one and your scheduler picks the
other, you'll see this.

The image needs only POSIX `sh` and `uname`. If you're using a
non-standard base, `docker run --rm -it your/image sh -c 'uname -m'`
to confirm.

## Other gotchas

### `cargo athena emit` shows old YAML after a code change

The cargo invocation behind `emit` uses `cargo run`, so an incremental
rebuild should pick up your changes. If it doesn't, you're probably
running from outside the workflow crate against a stale binary; pass
`-p <package>` or run from the crate root.

### "duplicate volume name" or DNS-1123 errors on `host!`

`host!("/p")` mounts under `/athena/mounts/<hash>` to avoid clobbering
the container fs (`host!("/")` would otherwise overlay the host root).
Two `host!` literals that hash to the same value would produce
duplicate Volume names, which Kubernetes rejects. The hash is a
stable FNV-1a 64-bit (16 hex chars), wide enough that collisions are
not practical.

If you need a specific in-container mount path, use
`#[container(host_mount = [...])]` instead.
