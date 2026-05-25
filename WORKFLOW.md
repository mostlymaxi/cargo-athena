# `#[workflow]`

A `#[workflow]` is an Argo DAG. Its body is **statically analyzed, not
executed**: each statement becomes an Argo task, data flow becomes a
DAG edge, and the function name becomes a `WorkflowTemplate`. The
entrypoint is a *type*:

```rust,ignore
fn main() { cargo_athena::entrypoint!(run_foo); }
```

The body is also type-checked as ordinary Rust. Wrong argument types
or arity, a missing struct field, consuming a workflow that has no
return, or calling a `#[fragment]` / regular function from a
`#[workflow]` are all **compile errors**.

## Attribute arguments

```rust,ignore
#[workflow(name = "...", steps,
           boundary_node_selector = { "k" = "v" },
           node_selector_if_root = { "k" = "v", "k2" = "lit" + arg },
           on_exit_if_root = path::to::template,
           retry(limit = 2, policy = "OnError", backoff = "30s"),
           ttl_if_root(after_completion = 86400, after_success = 3600, after_failure = 7200),
           pod_gc_if_root(strategy = "OnWorkflowSuccess"),
           active_deadline_if_root = "2h",
           mutexes = [{ name = "pipeline-dag" }],
           mutexes_if_root = [{ name = "deploy-" + env }])]
```

All are optional.

| Arg | Effect |
|---|---|
| `name = "my-name"` | Override the Argo template name. Default `<crate>-<fn>` (kebab). |
| `steps` | Emit an Argo `steps:` (sequential) template instead of the default data-dependency `dag:`. |
| `boundary_node_selector = { … }` | A `nodeSelector` constraint on pods whose immediate enclosing dag/steps is this template. Does NOT cascade through nested sub-workflows. Literal only. See [Node selector](#node-selector). |
| `node_selector_if_root = { … }` | Default `nodeSelector` for every pod in the submitted run. **Root-only**. Values support `"lit" + arg` / `"lit" + arg.field` injection of the workflow's own arguments. |
| `annotations = { "k" = "v" }` | Template-level annotations on the dag/steps template. Literal keys and values. |
| `on_exit_if_root = t` | Whole-workflow exit handler that fires only when *this* template is the workflow you submit. Distinct from the per-task `.on_exit(t)` builder. |
| `retry(limit, policy, backoff)` | Template-level retry. `limit` is required (`unlimited` means no cap); `policy` ∈ `Always` / `OnFailure` / `OnError` / `OnTransientError`; `backoff` is seconds or a [humantime](https://docs.rs/humantime) string. |
| `ttl_if_root(after_completion, after_success, after_failure)` | GC the finished Workflow after the given duration. At least one of the three is required. **Root-only.** |
| `pod_gc_if_root(strategy)` | Pod garbage collection. `strategy` ∈ `OnPodCompletion` / `OnPodSuccess` / `OnWorkflowCompletion` / `OnWorkflowSuccess`. **Root-only.** |
| `active_deadline_if_root = <dur>` | Whole-workflow runtime cap. The only timeout that works on a `#[workflow]`. **Root-only.** See [Timeouts](#timeouts). |
| `mutexes = [{ name, namespace }, …]` | Serialize this template against other holders of the same mutex name (within one run AND across separate Workflow runs). Both fields accept `"lit" + arg + arg.field` injection. See [Mutexes](#mutexes). |
| `mutexes_if_root = [{ name, namespace }, …]` | Serialize the whole submitted run against other runs holding the same mutex. **Root-only.** |

A parameter *name* (i.e. a function argument) or a `name = "…"`
value that a YAML 1.1 parser reads as a boolean/null (`y` / `yes` /
`n` / `no` / `on` / `off` / `true` / `false` / `null` / `~`, any
case) is a compile error: Argo's YAML→JSON parser would silently
mis-type it.

## Timeouts

To time-bound a whole workflow, use **`active_deadline_if_root`** -
the only mechanism Argo enforces at workflow scope. The other two
knobs (`timeout`, `pod_running_timeout`) are per-pod and live on
[`#[container]`](container.md#timeouts).

The `_if_root` suffix means the cap applies only when this
WorkflowTemplate is the workflow you actually submit; it is inert
when this template is referenced as a nested sub-workflow.

Every duration is an integer (seconds) or a
[humantime](https://docs.rs/humantime) string (`"90s"`, `"1h30m"`,
`"2d"`).

## Node selector

Three knobs at three scopes. Pick by reach:

- [`#[container(node_selector = …)]`](container.md): this one pod
  only. Supports `"lit" + arg` value injection of the container's
  own inputs.
- `#[workflow(boundary_node_selector = …)]`: pods whose **immediate**
  enclosing dag/steps is this template. Does NOT cascade through
  nested sub-workflows. Literal keys and values only.
- `#[workflow(node_selector_if_root = …)]`: every pod in the
  submitted run that doesn't have a tighter override. **Root-only**
  (inert when referenced as a sub-workflow). Values support
  `"lit" + arg` injection of the workflow's own arguments.

**`boundary_node_selector` is intentionally literal-only.** If you
want a value that depends on an argument, use `node_selector_if_root`:
it's the only `#[workflow]` knob where per-arg injection has clear,
predictable semantics. Hand-written `{{workflow.parameters.X}}` is
accepted in a literal as an eyes-open escape hatch, but the natural
mental model of "this template's arg" doesn't hold for the boundary
case, so the macro forbids `+ arg` there to avoid silent surprises.

```rust,ignore
#[workflow(
    boundary_node_selector = { "kubernetes.io/arch" = "amd64" },
    node_selector_if_root  = { "tier" = "platform",
                               "env"  = "prod-" + env },
)]
fn pipeline(env: String) { /* ... */ }
```

## Mutexes

At most one workflow / node holds a named mutex at a time, **per
namespace**. Two separate Workflow runs sharing a mutex name will
serialize against each other.

Two tiers, picked by reach:

- **`mutexes_if_root`** (root-only): held for the whole submitted
  run. The "one of these workflows at a time" knob. Inert when
  referenced as a sub-workflow.
- **`mutexes`** (template-level): held just while this template's
  node is running. Lets parallel tasks in one run serialize on a
  per-shard mutex name; lets a sub-workflow self-serialize wherever
  it's embedded.

Each entry is `{ name = …, namespace = … }`. `namespace` is optional
(defaults to the workflow's own namespace; set it explicitly to
coordinate across namespaces). Both fields accept the
`"lit" + arg + arg.field` injection grammar.

Cookbook: [Mutual exclusion across runs](cookbook.md#mutual-exclusion-across-runs).

## The body

Only three statement shapes are lowered:

```rust,ignore
let x = template(args);   // a task; `x` binds its output
template(args);           // a task (no output consumed)
if cond { ... } else { ... }  // see "if / else" below
```

Everything else (`match`, `for` / `while` / `loop`, macros,
arbitrary method calls, `let` with non-ident/tuple patterns,
`let … else`) is a **hard `compile_error!`** with a spanned message.
Nothing is silently dropped.

### Arguments to a template call

| Form | Semantics |
|---|---|
| literal `"s"`, `7`, `true` | a static parameter value |
| a `#[workflow]` input param | the workflow input, forwarded |
| a prior `let` binding | the producer's output **+ a DAG edge** |
| `binding.clone()` / `binding.to_owned()` | same as the binding (type-preserving) |
| `"lit".to_string()` / `"lit".into()` | same as the literal (**literal-only**) |
| `binding.field.sub` | one named field of the producer's output |
| a nested call `foo(bar())` | `bar` becomes its own task; recursive |

Notes:

- **`.clone()` is the fan-out marker.** Sending one binding to *two*
  consumers requires an explicit `.clone()` - which matches what
  Argo actually does (copy the parameter into each consumer).
- **`.to_string()` / `.into()` are literal-only.** On a binding or
  input they're rejected: they'd change the Rust type without
  changing the wire value, a silent mismatch. Any literal value is
  fine.

### Return values

A `#[workflow]` with a return type bubbles its **terminal** task's
output up, so a parent consumes a sub-workflow exactly like a
container:

```rust,ignore
#[workflow]
fn sub(seed: String) -> String {
    let fetched = fetch(seed);
    transform(fetched, 7)        // tail call == this workflow's return
}

#[workflow]
fn parent() {
    let r = sub("seed".to_string());
    publish(r);
}
```

The terminal is the tail template call, a returned/tail binding, or
a value-`if` (below). A return type with no resolvable terminal is a
compile error.

## Per-task builder chain

A task call may be suffixed, in any order, with:

```rust,ignore
fetch(url).continue_on(failed, error);          // dependents proceed on failure/error
transform(x).on_exit(cleanup);                  // unconditional per-task exit hook
transform(x).on_exit(record("done"));           // hook target may take args
transform(x).on_success(notify).on_failure(alarm);   // repeatable phase hooks
transform(x).on_error(alarm);
transform(x).hook_if("workflow.status == 'Failed'" = alarm);  // raw Argo expr escape hatch
```

- `.continue_on(failed | error | failed, error)`: at most one; lets
  dependents proceed even when this task fails / errors.
- `.on_exit(t)` / `.on_exit(t(args))`: at most one; an unconditional
  per-task exit hook.
- `.on_success(t)` / `.on_failure(t)` / `.on_error(t)`: repeatable;
  fire on the corresponding phase.
- `.hook_if("raw-argo-expression" = t, …)`: repeatable; verbatim
  Argo expression escape hatch.

Any hook target is `t` or `t(args)`. Hook templates are reachable
from this workflow, so they get registered like any other callee.

## `.fan_out(|x| C(x, …))` - list fan-out

`let b = a.fan_out(|x| caps(x, "!".to_string()));` runs `caps` once
per element of `a`. `b` is the aggregated `Vec<U>`, consumed
downstream like any output.

- `a` must be a prior `let` binding or a `#[workflow]` input that is
  a list.
- The closure body must be a single template call.
- Element / closure / result types are type-checked.

## `if` / `else` / `else if`

Real Rust conditionals run exactly one branch:

```rust,ignore
// statement-if / else-if / else
if n == 0 {
    note("zero".to_string());
} else if m.id == "abc" && n > 1 {
    note(chosen);
} else {
    note("other".to_string());
}

// value-if: pass the taken branch's value out
let chosen = if n > 3 { left(n) } else { right(n) };
```

- **Conditions** are a closed grammar: comparisons `== != < <= > >=`
  combined with `&&` / `||` / `!`. Operands are a binding, a
  `#[workflow]` input, an `a.field` of one, a literal, or a nested
  template call. Anything outside this grammar (method calls,
  arithmetic, casts) is a targeted compile error.
- **Value-`if`** requires an `else` and both arms producing the same
  type.
- Bindings created *inside* an arm are not visible after the `if`.
  Use the value-`if` form to pass a result out.

## Strict by design

If it compiles, the argument / field / return types line up and
every statement was lowered. There is no silent mis-emit.

## See also

Cookbook recipes that exercise these features:

- [Sequential vs. parallel](cookbook.md#sequential-vs-parallel)
- [Reuse a multi-step workflow as a building block](cookbook.md#reuse-a-multi-step-workflow-as-a-building-block)
- [Inline one step's output into another](cookbook.md#inline-one-steps-output-into-another)
- [Fan-out over a list](cookbook.md#fan-out-over-a-list)
- [Conditionals](cookbook.md#conditionals)
- [Pass only one field of a struct](cookbook.md#pass-only-one-field-of-a-struct)
- [Force a sequential execution order](cookbook.md#force-a-sequential-execution-order)
- [Per-task hooks](cookbook.md#per-task-hooks)
- [Retry with backoff](cookbook.md#retry-with-backoff)
- [Timeouts](cookbook.md#timeouts)
- [Whole-workflow cleanup](cookbook.md#whole-workflow-cleanup)
- [Mutual exclusion across runs](cookbook.md#mutual-exclusion-across-runs)
- [Pin every step in a workflow to specific nodes](cookbook.md#pin-every-step-in-a-workflow-to-specific-nodes)

Hitting an error? See [Troubleshooting](troubleshooting.md).
