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
           boundary_node_selector = { "k" = "v" },
           node_selector_if_root = { "k" = "v" },
           on_exit_if_root = path::to::template,
           retry(limit = 2, policy = "OnError", backoff = "30s"),
           ttl_if_root(after_completion = 86400, after_success = 3600, after_failure = 7200),
           pod_gc_if_root(strategy = "OnWorkflowSuccess"),
           active_deadline_if_root = "2h")]
```

| Arg | Effect |
|---|---|
| `name = "my-name"` | Override the Argo template name. Default: `<crate>-<fn>` (kebab). |
| `steps` | Emit an Argo `steps:` (sequential) template instead of the default data-dependency `dag:`. |
| `boundary_node_selector = { "k" = "v" }` | `nodeSelector` on this dag/steps template (`Template.NodeSelector`). Argo applies it to pods whose **immediate enclosing dag/steps is this template** — does NOT cascade through nested sub-workflows. For "every pod in the run", use `node_selector_if_root`. Literal keys *and* values. See [Node selector](#node-selector). |
| `node_selector_if_root = { "k" = "v" }` | `nodeSelector` on this WT's `WorkflowSpec.NodeSelector`. **Root-only**: applies to every pod in the run that doesn't have a template- or boundary-level override. Inert when this WT is `templateRef`'d as a sub-workflow. Same `_if_root` family as `ttl_if_root`/`pod_gc_if_root`/`active_deadline_if_root`. Literal keys *and* values. |
| `annotations = { "k" = "v" }` | Template-level annotations (`metadata.annotations`) on the dag/steps template. Literal keys *and* values (drop in `{{workflow.parameters.X}}` as a literal for dynamic). |
| `on_exit_if_root = t` | Whole-workflow exit handler on this template's own `spec.hooks.exit`. Fires only when *this* template is the workflow you submit. Distinct from the per-task `.on_exit(t)` builder. |
| `retry(limit = N \| unlimited, policy = "…", backoff = <dur>)` | Template-level Argo `retryStrategy`. `limit` is **required** (`unlimited` ⇒ no cap); `policy` ∈ `Always\|OnFailure\|OnError\|OnTransientError`; `backoff` is an int (seconds) or a [humantime](https://docs.rs/humantime) string. |
| `ttl_if_root(after_completion = <s>, after_success = <s>, after_failure = <s>)` | WorkflowSpec `ttlStrategy`: GC the finished Workflow. ≥1 of the three is required (int seconds or humantime). **Root-only.** |
| `pod_gc_if_root(strategy = "<S>")` | WorkflowSpec `podGC`. `strategy` ∈ `OnPodCompletion\|OnPodSuccess\|OnWorkflowCompletion\|OnWorkflowSuccess`. **Root-only.** |
| `active_deadline_if_root = <secs \| "2h">` | WorkflowSpec `activeDeadlineSeconds` — the whole-workflow runtime cap. The only timeout that works on a `#[workflow]`. **Root-only.** See [Timeouts](#timeouts). |

All are optional. A parameter *name* (i.e. a function argument) or a
`name = "…"` value that a YAML 1.1 parser reads as a boolean/null
(`y/yes/n/no/on/off/true/false`, `null`, `~`, any case) is a compile
error — Argo's YAML→JSON parser would silently mis-type it.

## Timeouts

To time-bound a whole workflow, use **`active_deadline_if_root`** —
the only mechanism Argo enforces at workflow scope. The other two
knobs (`timeout`, `pod_running_timeout`) are per-pod and live on
[`#[container]`](container.md#timeouts).

`_if_root` is load-bearing: like `ttl_if_root`/`pod_gc_if_root`, the
cap applies **only when this WorkflowTemplate is the workflow you
actually submit**. It is inert when this template is `templateRef`'d
as a nested sub-workflow.

Every duration is an integer (seconds) or a
[humantime](https://docs.rs/humantime) string (`"90s"`, `"1h30m"`,
`"2d"`).

## Node selector

Argo's nodeSelector lookup at pod-creation time is **3-tier** —
`tmpl.NodeSelector → boundary.NodeSelector → wfSpec.NodeSelector` —
and Argo **never walks the ancestor chain**, only the immediate
boundary (`workflow/controller/workflowpod.go:928-958`). That gives
us three distinct knobs, each at a different tier:

| Where you put it | Argo field | Reaches |
|---|---|---|
| [`#[container(node_selector = …)]`](container.md) | `Template.NodeSelector` on the container | **This pod only.** Wins over both other tiers. |
| `#[workflow(boundary_node_selector = …)]` | `Template.NodeSelector` on the dag/steps | Pods whose **immediate** enclosing dag/steps is this template. Does NOT cascade through nested sub-workflows. |
| `#[workflow(node_selector_if_root = …)]` | `WorkflowSpec.NodeSelector` | **Every pod in the run** that doesn't have a tmpl- or boundary-level override. Root-only (inert when this WT is `templateRef`'d). |

The "boundary-only / no cascade" behavior of the middle tier is
genuinely surprising — a `pipeline → sub → container` chain where
only `pipeline` sets `boundary_node_selector` does **not** carry the
selector down to `container`'s pod. Use `node_selector_if_root` when
you want a default for every pod regardless of nesting.

```rust,ignore
#[workflow(
    boundary_node_selector = { "kubernetes.io/arch" = "amd64" },
    node_selector_if_root  = { "tier" = "platform",
                                "region" = "{{workflow.parameters.region}}" },
)]
fn pipeline() { /* ... */ }
```

Workflow attrs are **literal-only** (keys *and* values) — a workflow
has no args to inject from. For a dynamic value, drop in
`{{workflow.parameters.<NAME>}}` as a literal (as in `region` above).
That's the only interpolation Argo keeps; it always resolves against
the **submitted root**, so supply `<NAME>` to the workflow you
actually submit, not to a sub-workflow.

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
