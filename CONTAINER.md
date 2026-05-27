# `#[container]`

A `#[container]` is a workflow step whose body is **ordinary Rust,
executed in a pod**. Unlike a `#[workflow]` (statically analyzed), a
container body really runs: arguments come in, the function executes,
and its return value goes out.

```rust,ignore
#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) -> String {
    println!("regular code, got: {a}");
    format!("done:{a}")
}
```

The image is arbitrary; it needs only POSIX `sh` and `uname`, so
distroless and read-only-rootfs images work fine.

## I/O contract

- Each function argument is one input parameter the step receives.
- The return value is the step's output; the next `#[workflow]` task
  consumes it like any binding.
- I/O is compile-time bound to `serde` (`DeserializeOwned` /
  `Serialize`). Borrows can't cross this boundary - take and return
  owned types (`String`, not `&str`).

## Large or binary return values

By default a step's return rides Argo as an inline parameter, which
is fine for small JSON. For large or binary payloads, wrap the value
in `cargo_athena::Artifact<T>` and it flows through your bucket
instead:

```rust,ignore
use cargo_athena::{container, Artifact};

#[container]
fn make_report() -> Artifact<Vec<u8>> {
    Artifact::new(build_pdf())
}

#[container]
fn ship(r: Artifact<Vec<u8>>) {
    upload(r.into_inner());
}
```

The wiring is automatic: any `#[workflow]` task that consumes an
`Artifact<T>`-typed value is hooked up via S3 instead of inline
parameters. Reach for it when the value is "big enough you'd worry
about it" - tens of KB or more, encoded binary blobs, file-shaped
data.

Plain returns and `Artifact<T>` returns mix freely inside the same
workflow; choose per step.

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
    pod_running_timeout = 600,
    ttl_if_root(after_completion = 86400, after_success = 3600, after_failure = 7200),
    pod_gc_if_root(strategy = "OnWorkflowSuccess"),
    active_deadline_if_root = "2h",
    mutexes = [{ name = "writer-" + shard }],
    mutexes_if_root = [{ name = "global-deploy" }],
)]
```

All optional.

| Arg | Effect |
|---|---|
| `image = "…"` | Container image. Default `[bootstrap].default_image` from `athena.toml`. |
| `name = "…"` | Override the Argo template name. Default `<crate>-<fn>` (kebab). |
| `service_account = "…"` | Pod `ServiceAccount`. Default `[defaults].service_account`. |
| `node_selector = { … }` | Pin pods of this template to nodes matching the labels. Literal keys; values may be injected (see [Parameter injection](#parameter-injection)). |
| `env = { "K" = "v", … }` | Extra container env entries the body reads via `std::env::var(…)`. Literal keys; values follow the same `"lit" + arg + …` injection grammar as `image`. |
| `host_mount = [{ host_path, mount_path, read_only }, …]` | Explicit hostPath mounts with chosen mount paths. Use when you really do want a specific in-container path (`/dev/shm`, sidecar data, …); `host!` is the safer form. `read_only` defaults to false. Dedup'd against `host!` paths on the same `host_path`. |
| `annotations = { "k" = "v", … }` | Pod-template annotations. Literal keys; values injectable like `env`. |
| `privileged = true` | K8s `securityContext.privileged: true` on this container. Off by default; opt in only when you really do need host devices / kernel-level access. Your cluster's PodSecurity admission still has the final say. |
| `on_exit_if_root = t` | Whole-workflow exit handler that fires only when *this* template is the workflow you submit. Distinct from the per-task `.on_exit(t)` builder. |
| `retry(limit, policy, backoff)` | Template-level retry. `limit` is required (`unlimited` means no cap); `policy` ∈ `Always` / `OnFailure` / `OnError` / `OnTransientError`; `backoff` is seconds or a [humantime](https://docs.rs/humantime) string. |
| `timeout = <dur>` | Per-step timeout that **counts Pending time**. See [Timeouts](#timeouts). |
| `pod_running_timeout = <dur>` | Per-step timeout that only counts time the pod is **Running**. |
| `ttl_if_root(after_completion, after_success, after_failure)` | GC the finished Workflow after the given duration. At least one of the three is required. **Root-only.** |
| `pod_gc_if_root(strategy)` | Pod garbage collection. `strategy` ∈ `OnPodCompletion` / `OnPodSuccess` / `OnWorkflowCompletion` / `OnWorkflowSuccess`. **Root-only.** |
| `active_deadline_if_root = <dur>` | Whole-workflow runtime cap. **Root-only.** See [Timeouts](#timeouts). |
| `mutexes = [{ name, namespace }, …]` | Serialize pods of this template against any other holder of the same mutex name (within one run AND across separate Workflow runs). Both fields accept `"lit" + arg + arg.field` injection. |
| `mutexes_if_root = [{ name, namespace }, …]` | Serialize the whole submitted run against other runs holding the same mutex. **Root-only.** |
| `tolerations = [{ key, operator, value, effect, toleration_seconds }, …]` | K8s `Toleration` list on this template's pod. Required: `key`, `operator` (`"Equal"` \| `"Exists"`), `effect` (`"NoSchedule"` \| `"PreferNoSchedule"` \| `"NoExecute"`). Optional: `value` (required only with `Equal`), `toleration_seconds` (NoExecute only). `key`, `value`, `effect` accept `"lit" + arg` injection. |
| `tolerations_if_root = [{ key, operator, value, effect, ... }, …]` | Same, but applied to every pod in the run (3rd tier of Argo's `tmpl → boundary → wfSpec` lookup). **Root-only.** |
| `affinity = "<json\|yaml>"` | Opaque YAML/JSON string for K8s pod affinity. Athena does NOT model the deeply-nested `apiv1.Affinity` schema by design — use `pod_spec_patch` if you'd rather express it patch-style. |
| `affinity_if_root = "<json\|yaml>"` | Same, but applied to every pod in the run. **Root-only.** Hand-write `{{workflow.parameters.X}}` substitutions inside the YAML body if you need dynamic values. |
| `pod_spec_patch = "<json\|yaml>"` | Strategic-merge patch applied to this template's pod just before submission. Universal escape hatch for any podSpec field athena doesn't have a first-class attr for (resources, sidecars, init containers, fsGroup, …). String accepts `"lit" + arg` injection. **Dangerous-by-design**: athena does NOT validate the patch shape; Argo / k8s reject malformed input at submit / admission time. |
| `pod_spec_patch_if_root = "<json\|yaml>"` | Same, but applied to **every** pod in the run. Argo concats with each template's own `pod_spec_patch`. **Root-only.** |
| `image_pull_secrets_if_root = ["regcred", …]` | Root-only `WorkflowSpec.ImagePullSecrets`. Secret names the kubelet uses to pull every pod's image from a private registry. K8s / Argo expose this only at workflow scope; per-container needs go through `pod_spec_patch`. |

### Per-argument `#[inject("...")]`

Mark a function parameter as filled by Argo's substitution rather than
a normal `inputs.parameters` entry. The expression is passed verbatim
to Argo — athena does NOT validate it (the user owns Argo's variable
scope rules + any JSON wrapping needed for the arg type).

```rust,ignore
#[container]
fn smart_retry(
    payload: String,                                  // normal input
    #[inject("{{retries}}")] attempt: i64,            // bare numeric
    #[inject("\"{{pod.name}}\"")] pod: String,        // quoted string
) { /* ... */ }
```

The container's `container.args[]` carries one positional slot per fn
parameter in declaration order; param args contribute
`{{inputs.parameters.<name>}}`, inject args contribute the user's raw
expression verbatim. The run-side decodes every slot the same way
(positional argv → `serde_json::from_str`), so:

- numeric / bool Rust types: bare expression (`{{retries}}` → `3` →
  `3_i64`)
- `String` (and friends): wrap in `"..."` yourself
  (`"\"{{pod.name}}\""` → `"podname"` → `String "podname"`)

Workflow bodies call the template without passing inject args — they're
invisible to the caller. Allowed anywhere in the signature.

This is **dangerous by design**: athena trusts the expression. Argo's
admission rejects unknown variables at submit; out-of-scope refs
(`{{tasks.X.outputs.parameters.Y}}` from a container submitted directly,
or `{{retries}}` outside a retry context) silently leave the placeholder
in place and panic the run-side decode. Power-user escape hatch.

As with `#[workflow]`, an argument *name* or a `name = "…"` value
that a YAML 1.1 parser reads as a boolean/null is a compile error.

## Timeouts

Three "stop after a while" knobs:

| Attribute | What it bounds | Clock starts |
|---|---|---|
| `timeout` | this step | when the node is created (**includes** Pending) |
| `pod_running_timeout` | this pod | when the pod is **Running** |
| `active_deadline_if_root` | the whole submitted run | at workflow start (**root-only**) |

A pod stuck Pending trips `timeout` but not `pod_running_timeout`.
The first two are `#[container]`-only and are rejected on a
`#[workflow]`. The only working whole-workflow timeout is
`active_deadline_if_root`.

Every duration accepts an integer (seconds) or a
[humantime](https://docs.rs/humantime) string (`"90s"`, `"1h30m"`,
`"2d"`).

## Parameter injection

`image`, `service_account`, `env`, `node_selector`, `annotations`,
and the mutex `name` / `namespace` **values** can splice in the
container's own arguments:

```rust,ignore
#[container(
    image           = "ghcr.io/acme/app:" + tag,            // arg
    service_account = "athena-" + tenant + "-runner",       // literal + arg + literal
    node_selector   = { "kubernetes.io/arch" = "amd64",
                        "disktype" = profile.disk },        // a named struct field
)]
fn run(tag: String, tenant: String, profile: Profile) { /* ... */ }
```

Rules:

- The value is a string literal, or a `+`-concatenation of string
  literals and operands. An operand is an argument (`tag`) or a
  named struct field of one (`profile.disk`, `a.b.c`; no `a.0` /
  `a[i]`).
- String-literal segments are emitted verbatim - a hand-written
  `{{workflow.parameters.x}}` inside a literal passes through
  untouched (eyes-open escape hatch).
- Operands must be `String` / `&str` or a number (`i64`, `f64`, …).
  Anything else is a compile error, because only those round-trip
  to a raw scalar value.
- `node_selector` keys are always literal (a dynamic label key
  would be a foot-gun).
- `name` is the static template identity and `on_exit_if_root` is
  a template path; neither is an injection target.

## `#[fragment]`

A `#[fragment]` is a **plain helper function**, not a template. It
is called as ordinary Rust, so it executes inside the calling
container's pod:

```rust,ignore
#[container(image = "ghcr.io/acme/tools:latest")]
fn build() {
    frag_a();
}

#[fragment]
fn frag_a() {
    let _ = cargo_athena::host!("/var/lib/a");
    frag_b();                                  // transitive
}

#[fragment]
fn frag_b() { let _ = cargo_athena::host!("/var/lib/b"); }
```

Its purpose is to **carry pod-resource declarations across function
boundaries**. Every `host!` / artifact-port / `secret!` a fragment
uses is added to each `#[container]` that transitively calls it. A
`#[fragment]` cannot be called from a `#[workflow]` (it is not a
template; doing so is a type error).

## Macro calls

These declare pod resources and are only valid inside a
`#[container]` or `#[fragment]`. Calling them anywhere else is a
compile error.

| Macro | Effect | Runtime value |
|---|---|---|
| `host!("/abs/path")` | a `hostPath` volume mounted at a safe path the macro picks. | `&'static Path` (the path your code reads/writes; use `path.join("file")`, `path.display()`, etc.) |
| `load_artifact!("key")` | S3 **input** at the given object key | `Vec<u8>` |
| `load_artifact_str!("key")` | same, as text | `String` |
| `save_artifact!("key", bytes)` | S3 **output** at the given object key | writes `impl AsRef<[u8]>` |
| `save_artifact_str!("key", text)` | same, as text | writes `impl AsRef<str>` |
| `secret!("name", "key")` | a K8s Secret env on this container | `String` (panics if unset) |
| `secret_opt!("name", "key")` | same, optional | `Option<String>` |

```rust,ignore
#[container]
fn publish(report: String) {
    let notes = cargo_athena::load_artifact_str!("notes");
    println!("publishing {report} (notes: {notes})");
    cargo_athena::save_artifact!("receipt", format!("ok:{report}"));
}
```

Key properties:

- **Literal key only.** The argument is the exact S3 object key
  (for artifacts) or absolute path (for `host!`) - a string literal
  or const, resolved at compile time.
- **Collected from every branch.** Declarations are picked up from
  *every* `if` / `match` / loop branch in the body, not just the
  one path that runs. The pod's spec is fixed before the pod
  starts, so this is the only correct behavior.
- **Artifacts are decoupled.** A producer and consumer that share
  only an S3 key have no DAG dependency or ordering. A missing
  object is a runtime error in the consumer.
- **Carried through fragments** transitively, as above.

Used path-qualified (`cargo_athena::host!`) by convention so it
doesn't require a `use` and the gating compile errors stay obvious.

## Async `#[container]`

Mark a container `async fn` and the macro wraps the body in a
current-thread tokio runtime built per invocation. Enable the
`tokio` feature on `cargo-athena` to opt in - `tokio` is re-exported.

```rust,ignore
// Cargo.toml: cargo-athena = { …, features = ["tokio"] }

#[container]
async fn fetch(url: String) -> String {
    cargo_athena::tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    format!("data-from:{url}")
}
```

`#[workflow]` bodies are statically analyzed, so
`#[workflow] async fn` is a compile error.

## See also

Cookbook recipes that exercise these features:

- [Share data between steps without a dependency](cookbook.md#share-data-between-steps-without-a-dependency)
- [Share data and keep a strict order](cookbook.md#share-data-and-keep-a-strict-order)
- [Retry with backoff](cookbook.md#retry-with-backoff)
- [Timeouts](cookbook.md#timeouts)
- [Mutual exclusion across runs](cookbook.md#mutual-exclusion-across-runs)
- [Pin a single pod (image, service account, node)](cookbook.md#pin-a-single-pod-image-service-account-node)
- [Pull a Kubernetes Secret as an env var](cookbook.md#pull-a-kubernetes-secret-as-an-env-var)
- [Reuse setup across containers](cookbook.md#reuse-setup-across-containers)
- [Async `#[container]` fns](cookbook.md#async-container-fns)

Hitting an error? See [Troubleshooting](troubleshooting.md).
