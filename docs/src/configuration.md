# `athena.toml`

`athena.toml` describes where the binary and artifacts live and a few
pod defaults. It is required by `cargo athena` for `build`/`emit` and is
**never read in-pod** (everything it controls is baked into the emitted
YAML). The nearest one walking up from the cwd is used (like
`Cargo.toml`); point at a specific file with `cargo athena -c FILE …`
or the `ATHENA_CONFIG` env var.

A complete example (the one the kind e2e uses):

```toml
[artifact_repository.s3]
endpoint = "minio.argo.svc.cluster.local:9000"
bucket = "athena-artifacts"
region = "us-east-1"
insecure = true                                    # plain HTTP (e.g. MinIO)
access_key_secret = { name = "athena-s3", key = "accessKey" }
secret_key_secret = { name = "athena-s3", key = "secretKey" }

[artifact]
key = "athena/bin/e2e/0.1.0/e2e.tar.gz"

[bootstrap]
targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]

[defaults]
service_account = "default"
# package = "my-workflows"   # so `cargo athena` needs no --package/-p
# bin     = "app"            # …or --bin, in a multi-bin crate
```

## `[artifact_repository.s3]`

The S3-compatible bucket holding both the binary tarball and every
`load_artifact!` / `save_artifact!` object. Emitted into each container
template as an Argo `s3{}` artifact source.

| Key | Meaning |
|---|---|
| `endpoint` | S3 endpoint (host:port). |
| `bucket` | Bucket name. |
| `region` | S3 region. |
| `insecure` | `true` for plain HTTP (e.g. local MinIO). |
| `access_key_secret` / `secret_key_secret` | Kubernetes `{ name, key }` secret selectors for credentials. |

## `[artifact]`

| Key | Meaning |
|---|---|
| `key` | The exact S3 object key for the binary `.tar.gz`. `cargo athena build` uploads here; `emit` references it. |

## `[bootstrap]`

| Key | Meaning |
|---|---|
| `targets` | The static-musl target triples to cross-compile. Each becomes `app-<triple>` in the tarball; in-pod the bootstrap picks the one matching `uname`. |
| `default_image` | (optional) Image for `#[container]`s with no explicit `image`. A multi-arch base with POSIX `sh`/`tar`/`uname`. |

## `[defaults]`

| Key | Meaning |
|---|---|
| `service_account` | Pod `ServiceAccount` for every container, unless overridden by `#[container(service_account = "…")]`. |
| `package` | (optional) Default cargo package the `cargo athena` subcommands drive, so you don't repeat `-p`/`--package`. The flag wins. |
| `bin` | (optional) Default cargo bin within it (multi-bin crates need this). The `--bin` flag wins. |

> The artifact bucket is the *only* coupling between an artifact's
> producer and consumer (see [`#[container]` → macro calls](container.md)):
> they share a key, not a DAG edge.
