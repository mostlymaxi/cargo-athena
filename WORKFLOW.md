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
#[workflow(name = "...", steps, on_exit = path::to::template)]
```

| Arg | Effect |
|---|---|
| `name = "my-name"` | Override the Argo template name. Default is `<crate>-<fn>` (kebab-case). |
| `steps` | Emit an Argo `steps:` template (one sequential group per statement, refs via `{{steps.X…}}`, no `dependencies`) instead of the default data-dependency `dag:`. |
| `on_exit = t` | Whole-workflow exit handler, carried on the **root `WorkflowTemplate`'s** `spec.hooks.exit.templateRef`, so it fires whichever way the root is run — `argo submit --from workflowtemplate/<root>` *or* a `workflowTemplateRef` Workflow (`--with-workflow`). Emit-root only; a non-root `on_exit` is inert (matches Argo's workflow scope). |

All are optional. A parameter *name* (i.e. a function argument) or a
`name = "…"` value that a YAML 1.1 parser reads as a boolean/null
(`y/yes/n/no/on/off/true/false`, `null`, `~`, any case) is a compile
error — Argo's YAML→JSON parser would silently mis-type it.

## The body

Only three statement shapes are lowered:

```rust,ignore
let x = template(args);   // a task; `x` binds its output
template(args);           // a task (no output consumed)
if cond { … } else { … }  // see "if / else" below
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
