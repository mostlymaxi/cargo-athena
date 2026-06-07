# Publishing from CI

Cross-compiling a static-musl multi-arch binary, packaging it, and uploading it
to object storage is a production publish flow you should not have to re-derive.
cargo-athena ships a composite GitHub Action, `athena-publish`, that wraps the
whole thing: it installs the Zig cross-compiler and the rustup musl targets,
installs the pinned `cargo athena` CLI, and runs `cargo athena publish` against
your `athena.toml`.

It works with **any S3-compatible store** : the access key, secret key, and endpoint
are all you provide, and the endpoint (which usually already lives in your
`athena.toml`) is what selects the provider.

## Quick start

Add one workflow to your repo. This validates the cross-compile on every PR
(`build`, no credentials needed) and uploads on release tags (`publish`):

```yaml
# .github/workflows/publish.yml
name: publish
on:
  push:
    tags: ["v*"]
  pull_request:

jobs:
  publish:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - uses: mostlymaxi/cargo-athena/.github/actions/athena-publish@v0.5.2
        with:
          command: ${{ github.event_name == 'pull_request' && 'build' || 'publish' }}
          package: my-workflow
          access-key: ${{ secrets.S3_ACCESS_KEY }}
          secret-key: ${{ secrets.S3_SECRET_KEY }}
          # endpoint: https://<account>.r2.cloudflarestorage.com  # only to override athena.toml
```

Pin the action to a release tag (`@v0.5.2`). The tag fixes the `cargo athena`
CLI version too, so there is nothing else to keep in sync.

## Credentials

Pass your storage credentials as repository secrets:

| Input | Purpose |
|---|---|
| `access-key` | S3 access key (any S3-compatible provider). |
| `secret-key` | S3 secret key. |
| `endpoint` | Optional. Overrides the `athena.toml` endpoint for the CI uploader only (split-horizon). Usually unnecessary. |
| `session-token` | Optional. AWS-STS temporary-credential token. |

Do **not** put credentials in `athena.toml` (its
`*_secret` entries are Kubernetes Secret references for the in-cluster side).
The action reads creds only from these inputs. Omit `access-key`/`secret-key`
entirely to use the runner's ambient identity (instance role / IRSA).

## Inputs

| Input | Default | Notes |
|---|---|---|
| `command` | `publish` | `publish` (cross-compile + upload) or `build` (package only, no creds). |
| `package` | sole package | Cargo package to build (`-p`). |
| `bin` | default bin | Cargo bin within the package. |
| `targets` | from `athena.toml` | Comma/space list; overrides the `[bootstrap].targets` matrix. |
| `config` | discovered | Path to `athena.toml` if not found by walking up. |
| `working-directory` | `.` | Must be your Cargo workspace root (where `./target` lives). |
| `tarball` | none | `publish` only: upload a prebuilt tarball verbatim (build-once / upload-many). |
| `rust-toolchain` | `stable` | Toolchain to install. |
| `doctor` | `true` | Run `cargo athena doctor` preflight first. |

Output: `s3-uri` is the `s3://bucket/key` the tarball was uploaded to (empty for
`command: build`).

## Notes

- **Run from the workspace root.** `cargo athena build` stages the tarball
  relative to the current directory while cargo emits to the workspace `target/`,
  so `working-directory` must be the workspace root. In a monorepo, keep the
  default and pass `package`.
- **Pre-installed CLI wins.** If `cargo-athena` is already on `PATH` (for
  example via Nix), the action uses it and skips the install.
- **Caching.** The CLI install is cached across runs, and your crate build is
  cached with `Swatinem/rust-cache`, so warm runs are fast.

Prefer doing this by hand, or want the underlying flags? See
[the CLI page](cli.md#publish).
