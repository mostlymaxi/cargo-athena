# Roadmap

Unprioritized backlog — things to pick up later. Design notes live in
[`CLAUDE.md`](CLAUDE.md); each item below is a sketch, not a spec.

- **Redesign `save_artifact!` / `load_artifact!`.** In its current form
  it isn't very useful or obvious that it loads/saves to *constant*
  paths. Make the artifact ports more configurable (paths, and the
  S3 key shape) so it's clear what's read/written where.

- **Async container functions.** A simple async wrapper behind a
  `tokio` feature flag so users don't hand-roll the runtime/boilerplate
  for an `async fn` `#[container]` body — the macro sets up the runtime
  and `block_on`s the body.

- **Cleaner template/binary versioning.** Users may want to keep
  multiple versions of a template alive in one cluster instead of
  overwriting the single one. Mechanically easy — append the binary
  version tag to the template name — but decide: opt-in vs default, how
  callers/`templateRef`s resolve a version, and how `submit`/`emit`
  surface it.

- **`cargo athena publish`.** Still a stub. Implement the upload
  (tarball → the `athena.toml` S3 key) so `build` → `publish` is one
  flow instead of a manual `mc/s3cmd/aws s3 cp`.

- **Richer metadata annotations on emitted templates.** Stamp athena
  metadata onto each emitted `WorkflowTemplate` (and `templateRef`s) —
  athena version, package name, and anything else worth tracking —
  under `cargo.athena/<key> = <value>` annotations.

- **Nix flake.** Add a flake for install + packaging (currently only a
  `nix develop` dev shell).

- **More Argo arg coverage (start with TTL).** A `ttl` arg for
  `#[workflow]`/`#[container]`, and generally better support for
  surfacing Argo features as attribute args (e.g. `ttlStrategy`,
  `activeDeadlineSeconds`, `retryStrategy`, `parallelism`, …).

- **Gate the docs-site publish on tags.** `.github/workflows/pages.yml`
  currently builds/deploys on every push to `main`, so the published
  site runs ahead of the released binary. Trigger it on `v*` tags
  (like `publish.yml`) instead, so docs match the released crate.
