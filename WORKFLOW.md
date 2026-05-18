# `#[workflow]`

A `#[workflow]` is an Argo DAG. Its body is **statically analyzed, not
executed**: each statement is lowered to an Argo task, data flow becomes
`templateRef` wiring and DAG edges, and the function name becomes a
`WorkflowTemplate`. The entrypoint is a *type*:

```rust,ignore
fn main() { cargo_athena::entrypoint::<run_foo>(); }
```

The body is also type-checked as ordinary Rust by a hidden, never-run
"ghost" copy: wrong argument types or arity, a missing struct field,
consuming a workflow that has no return, or calling a `#[fragment]` /
regular function from a `#[workflow]` are all **compile errors**, not
runtime surprises.

## Attribute arguments

```rust,ignore
#[workflow(name = "...", steps,
           node_selector = { "k" = "v", ... },
           on_exit_if_root = path::to::template,
           retry(limit = 2, policy = "OnError", backoff = "30s"),
           timeout = "1h",
           ttl_if_root(after_completion = 86400, after_success = 3600, after_failure = 7200),
           pod_gc_if_root(strategy = "OnWorkflowSuccess"))]
```

| Arg | Effect |
|---|---|
| `name = "my-name"` | Override the Argo template name. Default is `<crate>-<fn>` (kebab-case). |
| `steps` | Emit an Argo `steps:` template (one sequential group per statement, refs via `{{steps.X…}}`, no `dependencies`) instead of the default data-dependency `dag:`. |
| `node_selector = { "k" = "v" }` | Set `nodeSelector` on this dag/steps template. The Argo controller **cascades** it onto every task pod this workflow `templateRef`s. **Keys and values are literal strings only** — see [Node selector](#node-selector). |
| `on_exit_if_root = t` | Whole-workflow exit handler. Every workflow that sets it carries it on **its own** `WorkflowTemplate`'s `spec.hooks.exit.templateRef`. Argo runs exit hooks workflow-scoped: only the workflow you actually **submit** fires its handler — so `argo submit --from workflowtemplate/X` runs *X*'s handler; *X*'s handler stays inert when *X* is just a `templateRef`'d sub-step of a bigger run (submit it directly to get it). Distinct from the per-task `.on_exit(t)` builder, which is a different, always-fires task hook. |
| `retry(limit = N \| unlimited, policy = "…", backoff = "<dur>")` | Template-level Argo `retryStrategy` on this dag/steps template. `limit` is **required** (`unlimited` ⇒ unbounded, no `limit` field); `policy` ∈ `Always\|OnFailure\|OnError\|OnTransientError` (optional; Argo defaults to `OnFailure`); `backoff` a duration string (optional). Not re-stamped on synthesized `if`-wrapper templates (workflow-scoped-attr policy). |
| `timeout = "<dur>"` | Template-level Argo `timeout` (e.g. `"1h"`). Optional. |
| `ttl_if_root(after_completion = <s>, after_success = <s>, after_failure = <s>)` | WorkflowSpec-scoped Argo `ttlStrategy` (GC the finished Workflow after the given seconds). All three optional ints but **≥1 required**. **Root-only — applies only when this WorkflowTemplate is the workflow you actually submit; inert when used as a nested `templateRef`'d sub-workflow** (proven on real Argo v4.0.5; identical mechanism to `on_exit_if_root`). Not re-stamped on synthesized `if`-wrapper templates. |
| `pod_gc_if_root(strategy = "<S>")` | WorkflowSpec-scoped Argo `podGC`. `strategy` **required**, ∈ `OnPodCompletion\|OnPodSuccess\|OnWorkflowCompletion\|OnWorkflowSuccess`. **Root-only** (same as `ttl_if_root`): applies only to the submitted top-level workflow; inert when nested via `templateRef`. |

All are optional. A parameter *name* (i.e. a function argument) or a
`name = "…"` value that a YAML 1.1 parser reads as a boolean/null
(`y/yes/n/no/on/off/true/false`, `null`, `~`, any case) is a compile
error — Argo's YAML→JSON parser would silently mis-type it.

## Node selector

```rust,ignore
#[workflow(node_selector = {
    "kubernetes.io/arch" = "amd64",
    "topology.kubernetes.io/region" = "{{workflow.parameters.region}}",
})]
fn pipeline() { /* ... */ }
```

Unlike [`#[container(node_selector = …)]`](container.md), a workflow's
keys **and values are literal strings only — no `"lit" + arg` parameter
injection.** A `#[workflow]` is a DAG/steps template, not a pod: athena
puts the selector on the template and the Argo controller cascades it
onto every task pod the workflow `templateRef`s (proven on real Argo
v4.0.5). Per-arg injection cannot work here, because:

- a *template-scoped* `{{=fromJSON(inputs.parameters.…)}}` is cascaded
  **raw** — the child pod receives the literal string and Kubernetes
  rejects it as an invalid label value; and
- the only interpolation that survives the parent→child cascade is
  `{{workflow.parameters.<NAME>}}`, which **always** refers to the
  *submitted root workflow's* parameters — never this workflow's own
  inputs when it runs as a `templateRef`'d sub-step.

So a dynamic value is an **eyes-open escape hatch**: write a literal
containing `{{workflow.parameters.foo}}` yourself (as in `region` above)
and own the root-scoping — supply `foo` as a parameter of the workflow
you actually `argo submit`, not of this sub-workflow. Plain static
labels need no special handling.

## The body

Only three statement shapes are lowered:

```rust,ignore
let x = template(args);   // a task; `x` binds its output
template(args);           // a task (no output consumed)
if cond { ... } else { ... }  // see "if / else" below
```

Everything else — `match`, `for`/`while`/`loop`, macros, arbitrary
method calls, `let` with non-ident/tuple patterns, `let … else` — is a
**hard `compile_error!`** with a spanned message. Nothing is silently
dropped.

### Arguments to a template call

| Form | Lowers to |
|---|---|
| literal `"s"`, `7`, `true` | a static Argo parameter value |
| a `#[workflow]` input param | `{{inputs.parameters.<name>}}` |
| a prior `let` binding | `{{tasks.<dep>.outputs.parameters.return}}` **+ a DAG edge** |
| `binding.clone()` / `binding.to_owned()` | same as the binding (type-preserving) |
| `"lit".to_string()` / `"lit".into()` | same as the literal (**literal-only**) |
| `binding.field.sub` | `{{=toJSON(fromJSON(<src>)['field']['sub'])}}` (named struct fields; tuple/index access is not lowered) |
| a nested call `foo(bar())` | `bar` becomes its own task; `foo` takes a ref to it (recursive: `foo(bar(baz()))`) |

Notes:

- **`.clone()` is the fan-out marker.** The body is faithful Rust (real
  move semantics). Sending one binding to *two* consumers requires an
  explicit `.clone()` — which is exactly correct, since Argo copies the
  output parameter into each consumer.
- **`.to_string()` / `.into()` are literal-only.** On a binding/input
  they would change the Rust type while emit still passes the raw
  serialized parameter — a silent mismatch — so they are rejected there.
  Any literal value is fine (every parameter value is emitted as JSON,
  so a string like `"no"` is unambiguous).

### Return values

A `#[workflow]` with a return type bubbles its **terminal** task's
output up as the template's own `outputs.parameters.return`, so a parent
consumes a sub-workflow exactly like a container:

```rust,ignore
#[workflow]
fn sub(seed: String) -> String {
    let fetched = fetch(seed);
    transform(fetched, 7)        // tail call == this workflow's return
}

#[workflow]
fn parent() {
    let r = sub("seed".to_string());
    publish(r);                  // {{tasks.sub.outputs.parameters.return}}
}
```

The terminal is the tail template call, a returned/tail binding, or a
value-`if` (below). A return type with no resolvable terminal is a
compile error.

## Custom method calls

### Per-task builder chain

A task call may be suffixed, in any order, with:

```rust,ignore
fetch(url).continue_on(failed, error);          // dependents proceed on failure/error
transform(x).on_exit(cleanup);                  // unconditional per-task exit hook
transform(x).on_exit(record("done"));           // hook target may take args
transform(x).on_success(notify).on_failure(alarm);   // repeatable phase hooks
transform(x).on_error(alarm);
transform(x).hook_if("workflow.status == 'Failed'" = alarm);  // raw Argo expr escape hatch
```

- `.continue_on(failed | error | failed, error)` — ≤1; sets Argo
  `continueOn`.
- `.on_exit(t)` / `.on_exit(t(args))` — ≤1; the special unconditional
  `exit` hook.
- `.on_success(t)` / `.on_failure(t)` / `.on_error(t)` — repeatable;
  athena generates the Argo phase `expression`.
- `.hook_if("raw-argo-expression" = t, …)` — repeatable; verbatim Argo
  expression escape hatch.

Any hook target is `t` or `t(args)` (args resolved like task args). Hook
templates are force-linked and emitted like any callee.

### `.fan_out(|x| C(x, …))` — list fan-out

`let b = a.fan_out(|x| caps(x, "!".to_string()));` runs `caps` once per
element of `a` (Argo `withParam`; the closure parameter is `{{item}}`,
`{{item.field}}` for a field of it). `b` is the aggregated `Vec<U>`,
consumed downstream like any output.

- `a` (the source) must be a prior `let` binding or a `#[workflow]`
  input that is a list.
- the closure body must be a single template call.
- the element/closure/result types are checked by the ghost
  (`AthenaList<T>` is blanket-implemented for `Vec<T>`/`[T; N]`).

### `if` / `else` / `else if`

Real Rust conditionals lower to synthesized, `when`-gated wrapper
workflows; exactly one branch runs.

```rust,ignore
// statement-if / else-if / else
if n == 0 {
    note("zero".to_string());
} else if m.id == "abc" && n > 1 {
    note(chosen);
} else {
    note("other".to_string());
}

// value-if: the wrapper selects + returns the taken branch
let chosen = if n > 3 { left(n) } else { right(n) };
```

- **Conditions** are a closed grammar: comparisons `== != < <= > >=`,
  combined with `&&` / `||` / `!`. Operands are a binding, a
  `#[workflow]` input, an `a.field` of one, a literal, or a nested
  template call (`if foo() > 3` — `foo` runs as a parent task, since
  Rust evaluates the condition unconditionally). Anything outside this
  grammar (method calls, arithmetic, casts) is a targeted compile error.
- **Value-`if`** requires an `else` and both arms producing the same
  type — Rust enforces this, and the ghost inherits it.
- Bindings created *inside* an arm are not visible after the `if`
  (Argo has no phi node); use the value-`if` form to pass a result out.

## Type checking & strictness, in one line

The data flow is compiler-enforced and the body contract is fail-loud:
if it compiles, the argument/field/return types line up and every
statement was lowered — there is no silent mis-emit.
