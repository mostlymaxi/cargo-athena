# Cookbook

Common patterns, each a few lines. The full rules behind them are on
the [`#[workflow]`](workflow.md) and [`#[container]`](container.md)
pages.

**Data flow & shape**

- [Sequential vs. parallel](#sequential-vs-parallel)
- [Reuse a multi-step workflow as a building block](#reuse-a-multi-step-workflow-as-a-building-block)
- [Inline one step's output into another](#inline-one-steps-output-into-another)
- [Fan-out over a list](#fan-out-over-a-list)
- [Conditionals](#conditionals)
- [Pass only one field of a struct](#pass-only-one-field-of-a-struct)
- [Force a sequential execution order](#force-a-sequential-execution-order)

**Artifacts & data sharing**

- [Share data between steps without a dependency](#share-data-between-steps-without-a-dependency)
- [Share data and keep a strict order](#share-data-and-keep-a-strict-order)

**Resilience & lifecycle**

- [Per-task hooks](#per-task-hooks)
- [Retry with backoff](#retry-with-backoff)
- [Timeouts](#timeouts)
- [Whole-workflow cleanup](#whole-workflow-cleanup)
- [Mutual exclusion across runs](#mutual-exclusion-across-runs)
- [Throttle pods per workflow / per DAG](#throttle-pods-per-workflow--per-dag)

**Pod placement & access**

- [Pin a single pod (image, service account, node)](#pin-a-single-pod-image-service-account-node)
- [Pin every step in a workflow to specific nodes](#pin-every-step-in-a-workflow-to-specific-nodes)
- [Pull a Kubernetes Secret as an env var](#pull-a-kubernetes-secret-as-an-env-var)
- [Reuse setup across containers](#reuse-setup-across-containers)
- [Async `#[container]` fns](#async-container-fns)
- [Share a PVC across containers in a workflow](#share-a-pvc-across-containers-in-a-workflow)

[Pitfalls](#pitfalls)

---

## Sequential vs. parallel

Edges come from **data**, not statement order. Independent calls run
in parallel; a shared input creates the dependency:

```rust,ignore
#[workflow]
fn pipeline() {
    let a = ingest("src".to_string());   // a and b are independent:
    let b = probe();                     //   they run in parallel
    combine(a, b);                       // depends on BOTH, joins them
}
```

Need a strict order without a real data dependency? See
[Force a sequential execution order](#force-a-sequential-execution-order).

## Reuse a multi-step workflow as a building block

A `#[workflow]` with a return type can be consumed exactly like a
container: the parent gets the workflow's terminal output as a value.
Build pipelines out of smaller pipelines:

```rust,ignore
#[workflow]
fn sub(seed: String) -> String {
    let f = fetch(seed);
    transform(f, 7)                      // tail call is this workflow's return
}

#[workflow]
fn parent() {
    let r = sub("seed".to_string());
    publish(r);
}
```

## Inline one step's output into another

`foo(bar())` runs `bar` as its own task and feeds its output straight
into `foo`. Shorthand for `let x = bar(); foo(x);`:

```rust,ignore
#[workflow]
fn pipeline() {
    publish(transform(fetch("u".to_string()), 7));
}
```

Recursive: `foo(bar(baz()))` works the same way.

## Fan-out over a list

```rust,ignore
#[workflow]
fn batch() {
    let items = make_list();                       // -> Vec<String>
    let out = items.fan_out(|x| caps(x, "!".to_string()));
    summarize(out);                                // out: Vec<String>
}
```

`caps` runs once per element of `items`; `out` is the aggregated
`Vec`, consumed like any output.

## Conditionals

Real `if` / `else` / `else if`; a value-`if` selects the taken branch:

```rust,ignore
#[workflow]
fn gated() {
    let n = decide("hello".to_string());
    let chosen = if n > 3 { left(n) } else { right(n) };  // value-if
    if n == 0 {
        note("zero".to_string());
    } else {
        note(chosen);
    }
}
```

Conditions are a closed grammar (`== != < <= > >=`, `&& || !` over
bindings / inputs / `a.field` / literals / nested calls).

## Pass only one field of a struct

`a.field` (or `a.field.sub`) wires only that field to the next task:

```rust,ignore
#[derive(serde::Serialize, serde::Deserialize)]
struct Meta { id: String, n: i64 }

#[container] fn make_meta() -> Meta { Meta { id: "abc".into(), n: 7 } }
#[container] fn use_id(id: String) { println!("id={id}"); }

#[workflow]
fn pipeline() {
    let m = make_meta();
    use_id(m.id);                          // only `id` is wired through
}
```

Named fields only (no `a.0` / `a[i]`). The compiler checks that the
field exists and matches the consumer's type.

## Force a sequential execution order

Two ways:

1. **Thread a return value through.** Any return value creates a real
   data dependency, so the consumer waits for the producer:

   ```rust,ignore
   #[workflow]
   fn pipeline() {
       let token = step_a();    // -> String
       step_b(token);           // can't start until step_a returns
   }
   ```

2. **Use `steps` mode.** The default `#[workflow]` body is a DAG
   (edges from data deps). Adding `steps` emits a sequential template
   instead, one statement per group:

   ```rust,ignore
   #[workflow(steps)]
   fn pipeline() {
       let p = prepare("seed".to_string());
       finalize(p);
   }
   ```

   Same body, different shape on the wire.

## Share data between steps without a dependency

A producer and consumer that share only an S3 key. No ordering, no
DAG wiring:

```rust,ignore
#[container]
fn produce() { cargo_athena::save_artifact_str!("report", "hello"); }

#[container]
fn consume() {
    let r = cargo_athena::load_artifact_str!("report");
    println!("{r}");
}
```

A missing object is an error at runtime for the consumer.

## Share data and keep a strict order

The recipe above has no ordering. To chain artifact-producing
containers explicitly, bridge them with a return value: the artifact
key stays a literal, and the return-value gives Argo the edge it
needs:

```rust,ignore
#[container]
fn produce() -> String {
    cargo_athena::save_artifact_str!("report", "hello");
    "ok".to_string()                       // return value creates the edge
}

#[container]
fn consume(seq: String) {
    let r = cargo_athena::load_artifact_str!("report");
    println!("seq={seq}: {r}");
}

#[workflow]
fn pipeline() {
    let token = produce();
    consume(token);                        // edge: produce must finish first
}
```

## Pass a large value between steps

A plain return goes inline through Argo, which is fine for small
JSON. For payloads measured in tens of KB or more, or any binary
blob, wrap the return in `Artifact<T>` and the value flows through
your bucket instead. Wiring is unchanged:

```rust,ignore
use cargo_athena::{container, workflow, Artifact};

#[container]
fn make_report() -> Artifact<Vec<u8>> {
    Artifact::new(build_pdf())          // big binary
}

#[container]
fn ship(r: Artifact<Vec<u8>>) {
    upload(r.into_inner());
}

#[workflow]
fn pipeline() {
    let r = make_report();
    ship(r);                            // looks like any binding-to-arg
}
```

When to pick which:

- **Plain `T`** for small structured values - configuration, IDs,
  counts, modest JSON. Easy to see in the Argo UI.
- **`Artifact<T>`** for large or binary returns. No size cliff to
  worry about, but the value isn't inspectable from the workflow
  status without downloading the object.
- **`save_artifact!` / `load_artifact!`** (the two recipes above) for
  fixed, known S3 keys where the producer and consumer can be wired
  separately or out of band. `Artifact<T>` is the DAG-wired sibling
  for the common one-producer/one-consumer case.

## Per-task hooks

`.continue_on` / `.on_success` / `.on_failure` / `.on_error` /
`.on_exit` fire for one specific task:

```rust,ignore
#[workflow]
fn resilient() {
    let raw = fetch("u".to_string()).continue_on(failed, error);
    transform(raw, 9)
        .on_failure(alarm)
        .on_exit(cleanup);     // runs when *this task* finishes
}
```

For a single hook that runs once when the whole workflow ends, see
[Whole-workflow cleanup](#whole-workflow-cleanup) below.

## Retry with backoff

A flaky step retries itself:

```rust,ignore
#[container(retry(limit = 3, policy = "OnError", backoff = "30s"))]
fn fetch(url: String) -> String { /* … */ "ok".into() }
```

`limit` is required (`unlimited` for no cap); `policy` is one of
`Always`, `OnFailure`, `OnError`, `OnTransientError`; `backoff` is an
int (seconds) or a humantime string. Works on `#[workflow]` too.

## Timeouts

Three knobs for three scopes; stack as many as you need:

```rust,ignore
#[container(
    timeout = "5m",                       // counts Pending time
    pod_running_timeout = "2m",           // only counts time Running
)]
fn long_step() { /* … */ }

#[workflow(active_deadline_if_root = "1h")]   // whole-workflow cap (root-only)
fn pipeline() { /* … */ }
```

Full distinctions: [Timeouts](container.md#timeouts).

## Whole-workflow cleanup

`on_exit_if_root` runs once when the workflow finishes, but only for
the workflow you actually submit:

```rust,ignore
#[workflow(on_exit_if_root = teardown)]
fn pipeline() { /* … */ }
```

When `pipeline` is run directly, `teardown` fires at the end (either
`argo submit --from workflowtemplate/pipeline` or `cargo athena
submit pipeline` works). When `pipeline` is embedded as a sub-step
of a bigger run, its own `on_exit_if_root` stays inert; submit it
directly if you want the hook.

This is distinct from the per-task `.on_exit(t)` builder, which
always fires for that one task.

## Mutual exclusion across runs

Block two runs of a workflow from racing each other, or serialize
one expensive step within a run:

```rust,ignore
// Only one "deploy" workflow at a time across the namespace:
#[workflow(mutexes_if_root = [{ name = "deploy-" + env }])]
fn pipeline(env: String) { /* … */ }

// Serialize one expensive step; the rest of the DAG fans out normally:
#[container(mutexes = [{ name = "shard-" + shard }])]
fn writer(shard: String) { /* … */ }
```

Two tiers, picked by reach:

- **`mutexes_if_root`** is held for the whole submitted run.
  **Root-only**: inert when this WT is embedded as a sub. The
  standard "one of these workflows at a time" knob.
- **`mutexes`** is held just while the template's node is running.
  Fires anywhere the template is invoked (root or nested).

Each entry is `{ name = …, namespace = … }`; `namespace` is optional
(defaults to the workflow's own). Both fields accept `"lit" + arg`
injection.

## Throttle pods per workflow / per DAG

Cap how many pods Argo runs at once, either for the whole submitted
run or just under one dag/steps template:

```rust,ignore
// Cap the whole run to 4 concurrent pods; cap THIS DAG to 2 of its
// own direct children at a time (nested templates don't count):
#[workflow(parallelism = 2, parallelism_if_root = 4)]
fn pipeline() { /* … */ }
```

Two tiers, picked by reach:

- **`parallelism_if_root`** caps `WorkflowSpec.parallelism`, applied to
  every pod in the submitted run. **Root-only**: inert when this WT
  is embedded as a sub-workflow.
- **`parallelism`** caps `Template.parallelism`, applied only to
  children scheduled DIRECTLY by this dag/steps. Pods from nested
  templates aren't counted. Fires anywhere the template is invoked.

Both are literal `i64` and must be `> 0` (Argo's CRD enforces
`Minimum=1`; the `*int64` schema rejects substituted strings at
admission, so neither field supports argument injection).

## Pin a single pod (image, service account, node)

Static, or with a container argument spliced in:

```rust,ignore
#[container(
    image           = "ghcr.io/acme/heavy:" + tag,
    service_account = "athena-" + tenant + "-runner",
    node_selector   = { "kubernetes.io/arch" = "amd64",
                        "disktype" = profile.disk },
)]
fn heavy(tag: String, tenant: String, profile: Profile) -> String { tag }
```

Operands are an argument or a named struct field of one, and must
be `String` / `&str` / number. See
[Parameter injection](container.md#parameter-injection).

## Pin every step in a workflow to specific nodes

```rust,ignore
#[workflow(
    boundary_node_selector = {                       // literal-only
        "kubernetes.io/arch" = "amd64",
    },
    node_selector_if_root = {                        // injection allowed
        "tier" = "platform",
        "env"  = "prod-" + env,
    },
)]
fn pipeline(env: String) { /* ... */ }
```

- `boundary_node_selector` covers pods whose immediate enclosing
  dag/steps is this template. Does NOT cascade through nested
  sub-workflows. Literal only. If you want a value that depends on
  an argument, use `node_selector_if_root`.
- `node_selector_if_root` is the default for every pod in the
  submitted run that doesn't have a tighter override.
  **Root-only**: inert when this WT is embedded as a sub. Values
  accept `"lit" + arg` / `"lit" + arg.field` injection.

## Tolerate node taints + steer with affinity

Most clusters taint GPU / spot / dedicated nodes; pods need tolerations
to schedule there:

```rust,ignore
#[container(tolerations = [
    { key = "nvidia.com/gpu", operator = "Exists", effect = "NoSchedule" },
    { key = "spot", operator = "Equal", value = "true", effect = "NoSchedule" },
])]
fn train(input: String) { /* ... */ }
```

For "every pod gets these tolerations," use the root-level version:

```rust,ignore
#[workflow(tolerations_if_root = [
    { key = "dedicated", operator = "Equal", value = "ml-team", effect = "NoSchedule" },
])]
fn pipeline() { /* ... */ }
```

Affinity is a deeply-nested K8s shape; athena keeps it as an opaque
YAML/JSON string so you write what K8s already documents:

```rust,ignore
#[workflow(affinity_if_root = r#"
nodeAffinity:
  requiredDuringSchedulingIgnoredDuringExecution:
    nodeSelectorTerms:
      - matchExpressions:
          - key: node-pool
            operator: In
            values: [gpu-a100]
"#)]
fn pipeline() { /* ... */ }
```

Embed `{{workflow.parameters.X}}` substitutions verbatim if you need
dynamic values at root scope. Same goes for the container-level
`affinity = "..."`.

The boundary tier covers the case "all pods that live inside this
specific dag inherit these scheduling constraints" (`boundary_tolerations`
+ `boundary_affinity`, mirroring `boundary_node_selector`). Pods that
set their own override the inheritance; pods that don't pick up the
boundary's values. Literal only — Argo's boundary-copy makes
per-arg injection unsafe at this tier (use `*_if_root` for dynamic).

## Reach a podSpec field athena doesn't have an attr for

`pod_spec_patch = "<json|yaml>"` is the universal escape hatch — Argo
strategic-merges the patch onto the rendered pod just before
submission. Use it for any K8s field cargo-athena doesn't lift to a
first-class attr (CPU/memory limits, init containers, sidecars,
`fsGroup`, `runtimeClassName`, …).

```rust,ignore
// Per-container patch (pins this template's pod resources).
#[container(pod_spec_patch = r#"{
  "containers":[{"name":"main","resources":{
    "limits":{"cpu":"500m","memory":"512Mi"},
    "requests":{"cpu":"100m","memory":"128Mi"}
  }}]
}"#)]
fn heavy(input: String) { /* ... */ }

// Whole-workflow patch (every pod in the run).
#[workflow(pod_spec_patch_if_root = r#"{
  "terminationGracePeriodSeconds":120
}"#)]
fn pipeline() { heavy("x".to_string()); }
```

The string accepts the usual `"lit" + arg` injection grammar, e.g.
`pod_spec_patch = r#"{"containers":[{"name":"main","resources":{"limits":{"cpu":""# + cpu + r#""}}}]}"#`.
Operands lower to `{{=fromJSON(inputs.parameters[..])}}` at
template scope, `{{=fromJSON(workflow.parameters[..])}}` at root.

Athena does NOT validate the patch shape — that's the trade-off for
"any field." Argo and the K8s API reject malformed input at submit /
admission time.

## Pull images from a private registry

Bind one or more `imagePullSecrets` (Secret names in the workflow's
namespace) to every pod in the run:

```rust,ignore
#[workflow(image_pull_secrets_if_root = ["regcred", "harborcred"])]
fn pipeline() { build(); deploy(); }
```

K8s / Argo expose this only at workflow scope; if you need a
per-container override (rare), reach for `pod_spec_patch`.

## Inject an Argo built-in variable as a parameter

`#[inject("<argo expression>")]` on a function arg fills it from Argo's
substitution at pod-creation, bypassing `inputs.parameters` entirely.

```rust,ignore
#[container]
fn smart_retry(
    payload: String,                                  // normal caller arg
    #[inject("{{retries}}")] attempt: i64,            // bare numeric
    #[inject("\"{{pod.name}}\"")] pod_name: String,   // quoted string
) {
    println!("attempt {attempt} on pod {pod_name} with payload={payload}");
}

#[workflow]
fn pipeline() {
    // The workflow body passes only the caller-visible param. The two
    // inject args are filled by Argo in the pod.
    smart_retry("hello".to_string());
}
```

The macro does NOT validate the expression — it's piped to Argo
verbatim. You own:

- The variable's scope. `{{retries}}` only resolves inside a
  `retry(...)` strategy; `{{tasks.X.outputs.Y}}` only resolves inside
  a DAG context.
- JSON wrapping. The run-side decodes via `serde_json::from_str`, so
  numeric / `bool` types want bare expressions (`{{retries}}` → `3`),
  `String` types want explicit quotes
  (`"\"{{workflow.name}}\""` → `"wf-abc"`).

Wrong wrapping or out-of-scope refs panic the run-side decode with a
clear "deserialize container input" message — useful as a debugging
signal that the value didn't substitute.

## Pull a Kubernetes Secret as an env var

`secret!("secret-name", "key")` declares a Secret env on the
container and reads it back at runtime as a `String`. `secret_opt!`
is the no-panic flavour (returns `Option<String>`):

```rust,ignore
#[container]
fn fetch(url: String) -> String {
    let token = cargo_athena::secret!("api-tokens", "api");
    let trace = cargo_athena::secret_opt!("debug-creds", "trace");
    /* … use them … */
    String::new()
}
```

`secret_opt!` skips the env when the secret/key is missing, instead
of failing pod start.

## Reuse setup across containers

A `#[fragment]` is just a normal Rust function that runs inside the
calling container. It can take arguments, do real work, and return a
value, exactly like any helper. Its only superpower: every `host!` /
artifact / `secret!` declaration it makes is added to each container
that transitively calls it.

So you can wrap "open a database connection" once and hand the
connection back to every container that needs one:

```rust,ignore
#[fragment]
fn open_db() -> DbHandle {
    let user = cargo_athena::secret!("db-creds", "user");
    let pass = cargo_athena::secret!("db-creds", "password");
    let ca   = cargo_athena::host!("/secrets/db");          // /secrets/db
    DbHandle::connect(&user, &pass, &ca)
}

#[container]
fn migrate() {
    let db = open_db();             // mounts + env land on this pod
    db.run_migrations();
}

#[container]
fn nightly_audit() {
    let db = open_db();             // …and this one
    let n = db.flag_anomalies();
    println!("flagged {n}");
}
```

Each container that calls `open_db()` gets the `/secrets/db` mount
and the `DB_USER` / `DB_PASSWORD` env entries automatically. The
calling containers don't have to know what's inside the fragment.

## Set up tracing (or any pod-only init) once for every container

Your workflow binary doubles as the in-pod runner AND the local
introspector that `cargo athena emit` / `ls` / `describe` / `submit`
spawn. So putting `tracing_subscriber::fmt().init()` directly in
`main()` would also fire on every local CLI invocation -- harmless
for stdout logging, but a real footgun for OTLP exporters, metrics
push, anything that dials out or costs money.

Gate it with `cargo_athena::is_container_run()` so the setup only
runs in-pod:

```rust,ignore
fn main() {
    // None for `cargo athena emit` / `ls` / etc.; Some(_) in-pod.
    // The returned guard (if any) drops at end of main(), so any
    // tracing/OTLP flush you stick on Drop fires after the body.
    let _otel = cargo_athena::is_container_run().then(|| {
        tracing_subscriber::fmt()
            .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
            .init();
        OtelFlushGuard::new()
    });
    cargo_athena::entrypoint!(MyRoot);
}
```

The pattern works for anything you only want in-pod: a Prometheus
push gateway, a Sentry init, an audit-log open. Per-container span
scoping (one `tracing::info_span!` per body) isn't covered by this
pattern -- if you need it, open an issue.

## Async `#[container]` fns

Mark a container `async fn` and the macro wraps the body in a
current-thread tokio runtime. Enable the `tokio` feature on
`cargo-athena` to opt in; `tokio` is re-exported:

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

## Share a PVC across containers in a workflow

Declare a PVC as a unit struct, then mount it with `pvc!(Type)`
inside any `#[container]` / `#[fragment]`. Two flavors, picked by
who owns the PVC's lifetime:

```rust,ignore
// Per-workflow-run scratch space. Argo creates the PVC at workflow
// start and deletes it at workflow end (Argo's
// `WorkflowSpec.volumeClaimTemplates`).
#[cargo_athena::ephemeral_pvc(
    size = "10Gi",
    access_modes = ["ReadWriteMany"],
)]
pub struct BuildCache;

// Reference to a pre-existing PVC (managed out of band). athena
// never creates or deletes it.
#[cargo_athena::external_pvc(claim_name = "shared-data-pvc", read_only = true)]
pub struct SharedData;

#[container]
fn build() {
    let cache: &Path = cargo_athena::pvc!(BuildCache);
    std::fs::write(cache.join("output.bin"), b"hello").unwrap();
}

#[container]
fn analyze() {
    // Same type → same PVC. Two pods sharing a `ReadWriteMany`
    // ephemeral see each other's files.
    let cache: &Path = cargo_athena::pvc!(BuildCache);
    let bytes = std::fs::read(cache.join("output.bin")).unwrap();
    println!("read {} bytes", bytes.len());
}

#[workflow]
fn pipeline() {
    let _ = build();
    analyze();
}
```

Two consumers sharing the same `#[ephemeral_pvc]` concurrently
need `ReadWriteMany`. `ReadWriteOnce` is fine when only one pod
ever mounts it at a time, but a parallel fan-out over RWO will
fail the second pod's volume attach.

The mount path is opaque (`/athena/pvcs/<hash>`) and stable across
emit and run — always use the returned `&'static Path` value;
never hard-code the path.

**v1 caveat**: every `#[ephemeral_pvc]` linked into your binary
lands on every emitted WorkflowTemplate's `volumeClaimTemplates`.
Argo creates ALL of them per run, even if the submitted workflow
doesn't reach them. Keep one workflow per binary and define each
`#[ephemeral_pvc]` near its consumer to avoid cross-workflow
PVC churn. See `CONTAINER.md` for details.

## Pitfalls

- **Fan-out a value to two consumers needs `.clone()`.** The body
  is faithful Rust; each consumer gets its own copy of the
  upstream value, so the explicit clone is correct.
- **Workflow bodies are strict.** Loops, `match`, and arbitrary
  expressions are compile errors by design, so a step is never
  silently dropped. `if` / `else` / `else if`, nested calls, and
  the builder / `fan_out` chain *are* supported.
- **Parameter values are JSON.** Every value athena emits is JSON,
  so any string is safe (`t("no")` works) and a `String` `"7"`
  stays a string, not a number.

Hitting an actual error? See [Troubleshooting](troubleshooting.md).
