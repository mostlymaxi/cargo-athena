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

## Per-task hooks & resilience

```rust,ignore
#[workflow]
fn resilient() {
    let raw = fetch("u".to_string()).continue_on(failed, error);
    transform(raw, 9)
        .on_failure(alarm)
        .on_exit(cleanup);
}
```

## Pinning a pod (image / SA / node)

```rust,ignore
#[container(
    image = "ghcr.io/acme/heavy:latest",
    service_account = "athena-runner",
    node_selector = { "kubernetes.io/arch" = "amd64", "disktype" = "ssd" },
)]
fn heavy(x: String) -> String { x }
```

## Pitfalls

- **Fan-out a value to two consumers → `.clone()`.** The body is
  faithful Rust; Argo copies the output into each consumer, so the
  explicit clone is correct, not boilerplate.
- **No literal YAML-1.1 booleans as values.** `t("no")` is a compile
  error (`no` would be mis-typed by Argo's YAML parser). `true`/`false`
  are fine; for an actual `"no"` string, return it from a container.
- **Workflow bodies are strict.** Loops, `match`, arbitrary expressions
  are compile errors — by design, so nothing is silently dropped.
