# `athena.toml`

`athena.toml` describes where the binary and artifacts live and a few
pod defaults. It is required by `cargo athena` and is
**never read in-pod** (everything it controls is baked into the emitted
YAML).

It is resolved in order: `cargo athena -c FILE …`, then the
`ATHENA_CONFIG` env var, then the nearest `athena.toml` walking up from
the cwd (like `Cargo.toml`), then a global
`~/.config/cargo-athena/athena.toml` (honoring `$XDG_CONFIG_HOME`). The
global fallback is for consumers of an already-published workflow:
`submit` / `emit` / `ls` / `describe` work from anywhere without a
per-repo config. First match wins (no merging); a repo-local
`athena.toml` always shadows the global one.

A complete example (the one the kind e2e uses):

```toml
[artifact_repository.s3]
endpoint = "minio.argo.svc.cluster.local:9000"
bucket = "athena-artifacts"
region = "us-east-1"
insecure = true                                    # plain HTTP (e.g. MinIO)
access_key_secret = { name = "athena-s3", key = "accessKey" }
secret_key_secret = { name = "athena-s3", key = "secretKey" }

[bootstrap]
targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]

[defaults]
service_account = "default"
# package   = "my-workflows" # so `cargo athena` needs no --package/-p
# bin       = "app"          # …or --bin, in a multi-bin crate
# namespace = "argo"         # default namespace for `cargo athena submit`
```

## `[artifact_repository.s3]`

The S3-compatible bucket holding both the binary tarball and every
`load_artifact!` / `save_artifact!` object. Emitted into each container
template as an Argo `s3{}` artifact source.

The `cargo athena` CLI does not use `access_key_secret` /
`secret_key_secret`. For its credentials, see
[S3 credentials](cli.md#s3-credentials).

| Key | Meaning |
|---|---|
| `endpoint` | S3 endpoint (host:port). |
| `bucket` | Bucket name. |
| `region` | S3 region. |
| `insecure` | `true` for plain HTTP (e.g. local MinIO). |
| `virtual_host` | `true` for virtual-hosted-style addressing (`https://<bucket>.<endpoint>/<key>`). Needs Argo v4.1 or later. Default `false`. See the note below. |
| `access_key_secret` / `secret_key_secret` | Kubernetes `{ name, key }` secret selectors for credentials. |

> `virtual_host` sets the addressing style of the `cargo athena` S3
> client (`publish`, `submit`, `emulate`, `doctor`). It also sets Argo's
> `addressingStyle` on every artifact in the pod. Set it only for a
> provider that rejects path-style requests. Keep it unset for MinIO and
> most S3-compatible stores.
>
> When `virtual_host` is `false`, the CLI uses path-style
> (`https://<endpoint>/<bucket>/<key>`). In the pod, Argo selects the
> style: virtual-hosted for AWS, Google Cloud Storage, and Alibaba Cloud
> OSS endpoints, and path-style for all other endpoints.
>
> When the endpoint host is an IP address or `localhost` (for example a
> port-forward), the CLI always uses path-style.
>
> Do not set `virtual_host` on Argo v4.0, because v4.0 does not have the
> `addressingStyle` field. With the default Argo v4.0 install,
> `kubectl apply` and `cargo athena submit` reject the WorkflowTemplate.
> With `--argo-server`, `submit` does not fail, but Argo removes the field.
> See [supported versions](argo-versions.md).

The binary tarball's object key is `{crate}/<tag>/{bin}.tar.gz`, where
`<tag>` is the build-time [version tag](cli.md#versioning) - the kebab of
your crate's semver on a clean release build (`1.2.3` -> `1-2-3`), or
`dev-<slot>` for a dev build. So a new release (or a new dev slot) lands
under its own key and never clobbers another version's binary.

## `[bootstrap]`

| Key | Meaning |
|---|---|
| `targets` | The static-musl target triples to cross-compile. Each becomes `app-<triple>` in the tarball; in-pod the bootstrap picks the one matching `uname`. |
| `default_image` | (optional) Default image when a `#[container]` doesn't set its own `image`. Needs only POSIX `sh` and `uname` (distroless works). |

## `[defaults]`

| Key | Meaning |
|---|---|
| `service_account` | Pod `ServiceAccount` for every container, unless overridden by `#[container(service_account = "…")]`. |
| `package` | (optional) Default cargo package the `cargo athena` subcommands drive, so you don't repeat `-p`/`--package`. The flag wins. |
| `bin` | (optional) Default cargo bin within it (multi-bin crates need this). The `--bin` flag wins. |
| `namespace` | (optional) Default Kubernetes namespace for `cargo athena submit`. Precedence: `-n/--namespace` → `$ARGO_NAMESPACE` → this → `default`. |

> The artifact bucket is the *only* coupling between an artifact's
> producer and consumer (see [`#[container]` → macro calls](container.md)):
> they share a key, not a DAG edge.
