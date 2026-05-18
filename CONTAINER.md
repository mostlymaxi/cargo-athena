# `#[container]`

A `#[container]` is a workflow step whose body is **ordinary Rust,
executed in a pod**. Unlike a `#[workflow]` (statically analyzed), a
container's body really runs: arguments are deserialized from Argo
parameters, the function executes, and its return value is serialized
back out.

```rust,ignore
#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) -> String {
    println!("regular code, got: {a}");
    format!("done:{a}")          // -> outputs.parameters.return
}
```

It compiles to its own Argo `WorkflowTemplate` (a container template).
The image is arbitrary — it only needs a POSIX `sh`, `tar`, and `uname`
(works on distroless / read-only-rootfs images).

### I/O contract

- Each function argument is an Argo **input parameter**, deserialized
  (serde) from that parameter at pod start.
- The return value is serialized to **`outputs.parameters.return`**, so
  a `#[workflow]` consumes it as `{{tasks.<t>.outputs.parameters.return}}`.
- Container I/O is compile-time bound to `serde` (`DeserializeOwned` /
  `Serialize`). Borrows can't cross this boundary — take/return owned
  types (`String`, not `&str`).

### How it runs in-pod

`cargo athena publish` cross-compiles one static-musl, multi-arch
`.tar.gz` and uploads it to the S3 `ArtifactRepository` from
`athena.toml` (`build` packages it locally without uploading). `emit`
injects, into every container template, that tarball as an input
artifact plus a small `sh` bootstrap that `uname`s, picks the matching
`app-<triple>`, and `exec`s it with `--cargo-athena-template <name>`.
All athena paths live under a pod-scoped `emptyDir` at `/athena`.

## Attribute arguments

```rust,ignore
#[container(
    image = "ghcr.io/acme/app:latest",
    name = "...",
    service_account = "athena-runner",
    node_selector = { "kubernetes.io/arch" = "amd64", "disktype" = "ssd" },
    on_exit_if_root = path::to::template,
    retry(limit = 3, policy = "OnError", backoff = "30s"),
    timeout = "5m",
    ttl(after_completion = 86400, after_success = 3600, after_failure = 7200),
    pod_gc(strategy = "OnWorkflowSuccess"),
)]
```

| Arg | Effect |
|---|---|
| `image = "…"` | Container image. Default: `[bootstrap].default_image` from `athena.toml`. Arbitrary per-container. |
| `name = "…"` | Override the Argo template name. Default `<crate>-<fn>` (kebab). |
| `service_account = "…"` | Pod `ServiceAccount`. Default: `[defaults].service_account` from `athena.toml`. |
| `node_selector = { "k" = "v", … }` | Template-level `nodeSelector` (the Argo controller cascades it onto this template's pods). Keys are literal; values may be injected (below). |
| `on_exit_if_root = t` | Whole-workflow exit handler on this template's own `spec.hooks.exit` — Argo fires it only for the workflow you actually submit (workflow-scoped). Same semantics as `#[workflow(on_exit_if_root)]`; distinct from the per-task `.on_exit(t)` builder. |
| `retry(limit = N \| unlimited, policy = "…", backoff = "<dur>")` | Template-level Argo `retryStrategy`. `limit` is **required** (`unlimited` ⇒ unbounded, no `limit` field); `policy` ∈ `Always\|OnFailure\|OnError\|OnTransientError` (optional; Argo defaults to `OnFailure`); `backoff` a duration string (optional). |
| `timeout = "<dur>"` | Template-level Argo `timeout` (e.g. `"5m"`). Optional. |
| `ttl(after_completion = <s>, after_success = <s>, after_failure = <s>)` | WorkflowSpec-scoped Argo `ttlStrategy` (GC the finished Workflow after the given seconds). All three optional ints but **≥1 required**. Stamped on this template's own `WorkflowTemplate` `spec` (workflow-scoped, fires only for the workflow you submit — same semantics as `on_exit_if_root`). |
| `pod_gc(strategy = "<S>")` | WorkflowSpec-scoped Argo `podGC`. `strategy` **required**, ∈ `OnPodCompletion\|OnPodSuccess\|OnWorkflowCompletion\|OnWorkflowSuccess`. Same per-WT scoping as `ttl`. |

All optional. As with `#[workflow]`, an argument *name* or a `name = "…"`
value that a YAML 1.1 parser reads as a boolean/null is a compile error.

### Parameter injection

`image`, `service_account`, and `node_selector` **values** can splice in
the container's own arguments — Argo substitutes the real value into
those fields when the pod is created:

```rust,ignore
#[container(
    image           = "ghcr.io/acme/app:" + tag,            // arg
    service_account = "athena-" + tenant + "-runner",        // literal + arg + literal
    node_selector   = { "kubernetes.io/arch" = "amd64",      // literal value
                        "disktype" = profile.disk },         // a named struct field
)]
fn run(tag: String, tenant: String, profile: Profile) { /* ... */ }
```

Rules:

- The value is a string literal, or a `+`-concatenation of string
  literals and operands. An operand is an **argument** (`tag`) or a
  **named struct field of one** (`profile.disk`, `a.b.c` — named fields
  only, no `a.0`/`a[i]`).
- **String-literal segments are emitted verbatim.** A hand-written
  `{{workflow.parameters.x}}` inside a literal passes through untouched
  — the escape hatch if you know Argo's templating and want it raw.
- Operands must be `String`/`&str` or a number (`i64`, `f64`, …).
  That's enforced at compile time: anything else (a struct, `Vec`,
  `bool`, …) is an error, because only those round-trip to the obvious
  raw scalar. A non-argument identifier, a tuple/index field, or any
  other expression is also a targeted error.
- **`node_selector` keys are always literal.** (Argo *can* substitute
  them, but a dynamic label key is a foot-gun, so athena forbids it by
  design.)
- `name` is the static Argo template identity and `on_exit_if_root` is
  a template path — neither is an injection target.

Under the hood an operand lowers to
`{{=fromJSON(inputs.parameters['arg']['field'…])}}` — Argo evaluates it
to the raw scalar at pod creation. You don't need to know that; the
point is `image = "repo:" + tag` just works.

## `#[fragment]`

A `#[fragment]` is a **plain helper function** — *not* a template. It is
genuinely called as Rust, so it executes inside the calling
container's pod:

```rust,ignore
#[container(image = "ghcr.io/acme/tools:latest")]
fn build() {
    frag_a();                                  // ordinary call, runs in this pod
}

#[fragment]
fn frag_a() {
    let _ = cargo_athena::host!("/var/lib/a"); // resource carried to `build`
    frag_b();                                  // transitive
}

#[fragment]
fn frag_b() { let _ = cargo_athena::host!("/var/lib/b"); }
```

Its purpose is to **carry pod-resource declarations across function
boundaries**. Every `host!` / artifact-port macro a fragment uses is
collected onto each `#[container]` that transitively calls it (resolved
as a closure at emit time). A `#[fragment]` cannot be called from a
`#[workflow]` (it is not a `Template`, so it fails as a type error).

## Macro calls

These declare pod resources and are only valid inside a `#[container]`
or `#[fragment]` (the public form is a `compile_error!` anywhere else,
and a `#[workflow]` using one is a hard error):

| Macro | Effect | Runtime value |
|---|---|---|
| `host!("/abs/path")` | a `hostPath` volume mounted at that path | `&str` path |
| `load_artifact!("key")` | an Argo S3 **input** artifact port at the exact `athena.toml` object key | `Vec<u8>` |
| `load_artifact_str!("key")` | same, as text | `String` |
| `save_artifact!("key", bytes)` | an Argo S3 **output** artifact port | writes `impl AsRef<[u8]>` |
| `save_artifact_str!("key", text)` | same, as text | writes `impl AsRef<str>` |

```rust,ignore
#[container]
fn publish(report: String) {
    let notes = cargo_athena::load_artifact_str!("notes");   // S3 input port
    println!("publishing {report} (notes: {notes})");
    cargo_athena::save_artifact!("receipt", format!("ok:{report}"));
}
```

Key properties:

- **Literal key only.** The argument is the exact S3 object key (for
  artifacts) or absolute path (for `host!`) — a string literal/const,
  resolved at compile time.
- **Static AST union, not a trace.** Declarations are collected from
  *every* `if`/`match`/loop branch, not the one path that runs. This is
  correct, not approximate: Argo fixes the pod spec before the pod runs,
  so the union is the only expressible semantics.
- **Decoupled through the bucket.** Artifact producer and consumer share
  only the S3 key — there is no DAG dependency, no `{{tasks.…}}` wiring,
  and no ordering imposed. A missing object is an Argo error at run time.
- **Carried through `#[fragment]`s** transitively, as above.

Used path-qualified (`cargo_athena::host!`) by convention so it doesn't
require a `use` and the gating compile-errors stay obvious.
