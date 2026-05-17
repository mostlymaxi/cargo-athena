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
