# Cookbook

Common patterns, each a few lines. The full rules behind them are on the
[`#[workflow]`](workflow.md) and [`#[container]`](container.md) pages.

## Sequential vs. parallel

Edges come from **data**, not statement order. Independent calls run in
parallel; a shared input creates the dependency:

```rust,ignore
#[workflow]
fn pipeline() {
    let a = ingest("src".to_string());   // a and b have no relation:
    let b = probe();                     //   they run in parallel
    combine(a, b);                       // depends on BOTH -> joins them
}
```

Need a strict order without a data dependency? Make the dependency
explicit by threading a return value through.

## Sub-workflow that returns a value

A `#[workflow]` with a return type bubbles its terminal task's output up
like a container's:

```rust,ignore
#[workflow]
fn sub(seed: String) -> String {
    let f = fetch(seed);
    transform(f, 7)                      // tail call == this workflow's return
}

#[workflow]
fn parent() {
    let r = sub("seed".to_string());
    publish(r);
}
```

## Nested calls

`foo(bar())` lowers `bar` to its own task and wires `foo`'s arg to its
output — a shorthand for `let x = bar(); foo(x);`:

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
    let out = items.fan_out(|x| caps(x, "!".to_string()));  // Argo withParam
    summarize(out);                                // out: Vec<String>
}
```

`caps` runs once per element (`{{item}}`); `out` is the aggregated
`Vec`, consumed like any output.

## Conditionals

Real `if` / `else` / `else if` become `when`-gated wrappers; a
value-`if` selects the taken branch:

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
bindings/inputs/`a.field`/literals/nested calls).

## Sequential `steps` mode

The default workflow body is a DAG (edges from data deps). Add `steps`
to emit an Argo `steps:` template — one statement per sequential
group:

```rust,ignore
#[workflow(steps)]
fn pipeline() {
    let p = prepare("seed".to_string());
    finalize(p);
}
```

Same body, different emit shape.

## Passing one field of a struct

`a.field` (or `a.field.sub`) wires only that field to the next task —
lowered to a JSON path on the producer's output:

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

Named fields only (no `a.0` / `a[i]`). The ghost type-checks that the
field exists and matches the consumer.

## Decoupled artifacts (no DAG edge)

A producer and consumer that share only an S3 key — no ordering, no
wiring:

```rust,ignore
#[container]
fn produce() { cargo_athena::save_artifact_str!("report", "hello"); }

#[container]
fn consume() {
    let r = cargo_athena::load_artifact_str!("report");
    println!("{r}");
}
```

## Artifact chain (with a DAG dep)

The recipe above has no ordering. To *chain* artifact-producing
containers — explicit dependency, guaranteed ordering — bridge them
with a return value:

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

The artifact key is still a literal; the return-value just orders the
two tasks.

## Per-task hooks & resilience

`.continue_on` / `.on_success` / `.on_failure` / `.on_error` /
`.on_exit` are *per-task* builders — they fire for that one task,
always:

```rust,ignore
#[workflow]
fn resilient() {
    let raw = fetch("u".to_string()).continue_on(failed, error);
    transform(raw, 9)
        .on_failure(alarm)
        .on_exit(cleanup);     // runs when *this task* finishes
}
```

## Retry with backoff

A flaky step retries itself:

```rust,ignore
#[container(retry(limit = 3, policy = "OnError", backoff = "30s"))]
fn fetch(url: String) -> String { /* … */ "ok".into() }
```

`limit` is required (`unlimited` for no cap); `policy` ∈
`Always|OnFailure|OnError|OnTransientError`; `backoff` is an int
(seconds) or a humantime string. Works on `#[workflow]` too.

## Timeouts

Three knobs for three scopes — stack as many as you need:

```rust,ignore
#[container(
    timeout = "5m",                       // controller; counts Pending time
    pod_running_timeout = "2m",           // kubelet; only counts time Running
)]
fn long_step() { /* … */ }

#[workflow(active_deadline_if_root = "1h")]   // whole-workflow cap (root-only)
fn pipeline() { /* … */ }
```

Full distinctions: [Timeouts](container.md#timeouts).

## Whole-workflow cleanup

`on_exit_if_root` is the *workflow-level* exit handler — distinct from
the per-task `.on_exit(t)` above. It runs once, when the workflow
finishes, but only for the workflow you actually submit:

```rust,ignore
#[workflow(on_exit_if_root = teardown)]
fn pipeline() { /* … */ }
```

Every `#[workflow]`/`#[container]` that sets it carries the hook on its
*own* template, so `argo submit --from workflowtemplate/pipeline` runs
`teardown`. When `pipeline` is instead a `templateRef`'d sub-step of a
bigger run, *its* `on_exit_if_root` stays inert (Argo fires only the
submitted workflow's) — submit it directly to get it.

## Pinning a pod (image / SA / node)

Static, or with a container argument spliced in (`image` /
`service_account` / `node_selector` *values* accept `"lit" + arg`):

```rust,ignore
#[container(
    image           = "ghcr.io/acme/heavy:" + tag,          // arg injected
    service_account = "athena-" + tenant + "-runner",
    node_selector   = { "kubernetes.io/arch" = "amd64",
                        "disktype" = profile.disk },          // a struct field
)]
fn heavy(tag: String, tenant: String, profile: Profile) -> String { tag }
```

Operands are an argument or a named struct field of one, and must be
`String`/`&str`/number. See
[`#[container]` → Parameter injection](container.md).

## Workflow-level node pinning

A `#[workflow]` exposes two nodeSelector knobs at two different tiers
of Argo's pod-creation fallback (`tmpl → boundary → wfSpec` — Argo
never walks the ancestor chain):

```rust,ignore
#[workflow(
    boundary_node_selector = {                       // literal-only
        "kubernetes.io/arch" = "amd64",
    },
    node_selector_if_root = {                        // injection allowed
        "tier" = "platform",
        "env"  = "prod-" + env,                      // arg injected
    },
)]
fn pipeline(env: String) { /* ... */ }
```

* `boundary_node_selector` lands on the dag/steps template
  (`Template.NodeSelector`). It only reaches pods whose **immediate**
  enclosing dag/steps is this template — does **not** cascade through
  nested sub-workflows. Keep it static; per-arg injection here would
  silently resolve against the submitted root's args, not this
  template's inputs.
* `node_selector_if_root` lands on this WT's
  `WorkflowSpec.NodeSelector` — the default for every pod in the run
  that doesn't have a tmpl- or boundary-level override. **Root-only**:
  inert when this WT is `templateRef`'d as a sub-workflow. Values
  accept the same `"lit" + arg` / `"lit" + arg.field` injection as
  `#[container]` attrs (lowered to
  `{{=fromJSON(workflow.parameters['arg'])}}`), so each `arg` is the
  workflow's own input as passed via `argo submit -p arg=...` or
  `cargo athena submit`.

See [`#[workflow]` → Node selector](workflow.md#node-selector) for the
full 3-tier table and why injection is asymmetric.

## Pull a K8s Secret as an env var

`secret!("secret-name", "key")` declares a Kubernetes Secret env on
the container template and reads it back at runtime as a `String`.
`secret_opt!` is the no-panic flavour (returns `Option<String>`).

```rust,ignore
#[container]
fn fetch(url: String) -> String {
    let token = cargo_athena::secret!("api-tokens", "api");
    let trace = cargo_athena::secret_opt!("debug-creds", "trace");
    /* … use them … */
    String::new()
}
```

Like `host!`, declarations are collected statically and carried through
`#[fragment]` callees, so a shared helper can stamp the same secret onto
every container that uses it:

```rust,ignore
#[fragment]
fn with_db_creds() { let _ = cargo_athena::secret!("db-creds", "password"); }

#[container]
fn migrate() { with_db_creds(); /* db-creds/password env now lives here */ }
```

Each declaration adds one `env` entry with `valueFrom.secretKeyRef`
on the container template. `secret_opt!` adds `optional: true` so
Argo skips the entry instead of failing pod-start when the secret/key
is missing.

## Async `#[container]` fns

Mark a container `async fn` and the macro wraps the body in a tokio
runtime (current-thread, built per invocation). Enable the `tokio`
feature on `cargo-athena` to opt in — `tokio` is re-exported.

```rust,ignore
// Cargo.toml: cargo-athena = { …, features = ["tokio"] }

#[container]
async fn fetch(url: String) -> String {
    cargo_athena::tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    format!("data-from:{url}")
}
```

`#[workflow]` bodies are statically analyzed, so `#[workflow] async fn`
is a compile error — `.await` is meaningless there.

## Shared pod resources via `#[fragment]`

A `#[fragment]` is a normal helper that runs inside the calling
container. Every `host!` / artifact decl it makes is carried onto
each container that transitively calls it — share pod resources
without a registry:

```rust,ignore
#[fragment]
fn with_secrets() {
    let _ = cargo_athena::host!("/secrets/db");
    let _ = cargo_athena::host!("/secrets/api");
}

#[container]
fn step_a() { with_secrets(); /* both /secrets paths land on step_a */ }

#[container]
fn step_b() { with_secrets(); /* …and on step_b */ }
```

Resources travel with the function; the calling containers don't have
to know what's inside.

## Pitfalls

- **Fan-out a value to two consumers → `.clone()`.** The body is
  faithful Rust; Argo copies the output into each consumer, so the
  explicit clone is correct, not boilerplate.
- **Workflow bodies are strict.** Loops, `match`, and arbitrary
  expressions are compile errors — by design, so a step is never
  silently dropped. `if`/`else`/`else if`, nested calls, and the
  builder/`fan_out` chain *are* supported.
- **Parameter values are JSON.** Every value athena emits is JSON, so
  any string is safe (`t("no")` works) and a `String` `"7"` stays a
  string, not a number.
