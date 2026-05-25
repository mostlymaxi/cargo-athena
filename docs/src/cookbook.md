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

**Pod placement & access**

- [Pin a single pod (image, service account, node)](#pin-a-single-pod-image-service-account-node)
- [Pin every step in a workflow to specific nodes](#pin-every-step-in-a-workflow-to-specific-nodes)
- [Pull a Kubernetes Secret as an env var](#pull-a-kubernetes-secret-as-an-env-var)
- [Reuse setup across containers](#reuse-setup-across-containers)
- [Async `#[container]` fns](#async-container-fns)

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
