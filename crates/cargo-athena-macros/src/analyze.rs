//! Workflow body analyzer.
//!
//! Parses a `#[workflow]` body into a [`Vec<Node>`] (the DAG/steps task
//! list) and a terminal [`Arg`] reference (the workflow's own
//! `outputs.parameters.return`). The output is consumed by
//! [`crate::node_tokens::node_tokens`] (which lowers each [`Node`] into
//! the `quote!` block builders run at emit time) and by
//! [`crate::conditional::synth_if`] (which synthesizes `if`/`else`
//! wrappers).
//!
//! Strict by design: every statement that isn't `let x = template(args);`,
//! `let x = <constant>;`, `template(args);`, or `if`/`else if`/`else`
//! becomes a spanned `compile_error!`. Argument resolution is equally
//! strict: see [`expr_to_arg`].

use proc_macro2::{Group, Span, TokenStream as TokenStream2, TokenTree};
use quote::quote;
use syn::spanned::Spanned;
use syn::visit_mut::VisitMut;
use syn::{Expr, ItemFn, Path, Stmt};

use crate::conditional::{SynthCtx, SynthWf, synth_if};
use crate::utils::{fn_args, is_builder_method, is_ctor, kebab, unwrap_expr};

pub(crate) enum Arg {
    /// Literal value, JSON-encoded at macro time.
    Lit(String),
    /// A constant value, serialized in-binary at emit (see
    /// `node_tokens::value_tokens`).
    Const(Expr),
    /// Reference to a previous task's output (creates a DAG dependency).
    Ref(String),
    /// Reference to one of the workflow's own input parameters.
    Input(String),
    /// Named-field access into a serde value: `a.b.c` lowered via Argo
    /// expr-templating `{{=toJSON(fromJSON(<src>)['b']['c'])}}` (the
    /// universal-safe form — see `node_tokens`). The ghost has already
    /// type-checked that the field path is valid on the producer's type.
    Json { src: JsonSrc, path: Vec<String> },
    /// The fan-out closure parameter: `|x| C(x)` → `{{item}}`,
    /// `|x| C(x.f)` → `{{item.f}}` (only valid on a `fan_out` node).
    Item { path: Vec<String> },
    /// Consuming a `fan_out` binding's **aggregate**. Argo's `withParam`
    /// aggregate is an array of each per-item task's `return` param,
    /// which is *already JSON-encoded* (Regime B) — so a plain
    /// `{{tasks.b.outputs.parameters.return}}` ref double-encodes (a
    /// `Vec<String>` comes back quote-wrapped). Emit a per-element
    /// re-normalizing expr instead (the array analog of the `Json`
    /// `toJSON(fromJSON(..))` universal-safe form). Carries the
    /// producing `fan_out` task (adds the DAG dep, like `Ref`).
    FanAgg(String),
    /// `Ref` to a `.continue_on` task: `Result<T, ArgoError>` encoding.
    RefFallible(String),
    /// `FanAgg` of a `.continue_on` fan: all-or-nothing
    /// `Result<Vec<T>, ArgoError>`. Carries the fan's list source so
    /// emit can compare aggregate length against source length (group
    /// `status` is absent from scope on Argo 3.6).
    FanAggFallible { task: String, src: FanSrc },
}

/// Where a `Json` arg's root value comes from.
pub(crate) enum JsonSrc {
    /// A prior `let` binding → the producing task (adds a DAG dep).
    Task(String),
    /// A `#[workflow]` input parameter (no dep).
    Input(String),
}

/// The list a `fan_out` iterates (Argo `withParam`).
#[derive(Clone)]
pub(crate) enum FanSrc {
    /// Prior `let` binding (Vec/array from a task) → `{{tasks.<t>.…}}`
    /// (adds a DAG dep on the producer).
    Task(String),
    /// A `#[workflow]` input list parameter → `{{inputs.parameters.<n>}}`.
    Input(String),
}

/// When a hook fires. The concrete Argo `expression` is generated in
/// `node_tokens` (it needs the task name + dag/steps scope, unknown at
/// peel time); `Exit` is the special unconditional `exit` hook.
#[derive(Clone)]
pub(crate) enum HookWhen {
    Exit,
    Success,
    Failure,
    Error,
    /// `.hook_if("raw-argo-expression" = t)` — verbatim Argo expr.
    Raw(String),
}

/// A hook peeled off a call, args still as raw `Expr` (resolved against
/// bindings/inputs later in `push_call`).
pub(crate) struct HookSpec {
    pub(crate) when: HookWhen,
    /// Hook template path — force-linked + emitted as a `templateRef`
    /// exactly like a callee.
    pub(crate) template: Path,
    /// Args to the hook template (`t(args)`); empty for a bare path.
    pub(crate) raw_args: Vec<Expr>,
}

/// Per-task builders peeled off a call: `.continue_on(...)`, `.on_exit`,
/// `.on_success`/`.on_failure`/`.on_error`, `.hook_if(...)`.
#[derive(Default)]
pub(crate) struct NodeOpts {
    /// `(error, failed)` for Argo `continueOn`.
    pub(crate) continue_on: Option<(bool, bool)>,
    pub(crate) hooks: Vec<HookSpec>,
}

/// A hook with its args resolved to `Arg`s (post `push_call`).
pub(crate) struct Hook {
    pub(crate) when: HookWhen,
    pub(crate) template: Path,
    pub(crate) args: Vec<Arg>,
}

pub(crate) struct Node {
    pub(crate) task: String,
    /// Callee path exactly as written (`ingest`, `foo::ingest`, …) — used
    /// as a *type* in `<callee as Template>` so the compiler resolves its
    /// Argo name/inputs across modules and crates and force-links it.
    pub(crate) callee: Path,
    pub(crate) args: Vec<Arg>,
    pub(crate) continue_on: Option<(bool, bool)>,
    pub(crate) hooks: Vec<Hook>,
    /// `Some` ⇒ this is a `fan_out` task (Argo `withParam` over the
    /// source list; the callee runs once per `{{item}}`).
    pub(crate) fan: Option<FanSrc>,
    /// `Some` ⇒ a fully-rendered Argo `when` expression (the task runs
    /// only if it holds). Set on the arm tasks of a synthesized `if`
    /// wrapper; `None` for ordinary unconditional tasks.
    pub(crate) when: Option<String>,
}

pub(crate) const UNSUPPORTED_ARG: &str = "unsupported argument in a #[workflow] call. \
Allowed: a literal; a #[workflow] input or a binding from a prior \
`let x = template(...);` (also `.clone()`, `.to_owned()`, `.field`); a nested \
template call; or a constant value: literals, `const` items, constant `let` \
bindings, struct / tuple / array literals, enum variants and tuple structs \
(`None`, `Some(..)`, `Kind::A(..)`), `.field`, `-`, `.clone()` / `.to_owned()` / \
`.to_string()` / `.to_vec()`, `.into()` on a string literal, and `const { .. }` \
blocks. Arithmetic, casts, indexing, macros and other calls aren't lowered: \
compute the value in a `const` item, a `const { .. }` block or a #[container].";

pub(crate) const CONST_CALL: &str = "a call inside a constant value must be an enum \
variant or tuple struct (`Some(..)`, `Kind::A(..)`: a name that starts with an \
uppercase letter). To use a task result, pass it to a #[container] that builds \
the value. Compute other values in a `const` item or a `const { .. }` block.";

pub(crate) const NESTED_CALL: &str = "a nested template call isn't lowered in a hook \
or `fan_out` arg. Bind it first (`let x = template(..);`) and pass `x`.";

pub(crate) const CONST_INTO: &str = "`.into()` in a constant value works only on a \
string literal. Use `.to_string()` for text and `[a, b].to_vec()` for a list.";

pub(crate) const LET_SHAPE: &str = "a #[workflow] `let` must bind a template call \
(`let x = template(args);`), an `if`, or a constant value (`let k = 3;`, \
`let cfg = Opts { .. };`). To reuse an input or a task result, pass it directly.";

pub(crate) const CONST_LET_TYPE: &str = "a constant `let` can't have a type \
annotation. Use a typed literal (`5_000_000_000u64`) or a `const` item.";

fn mixed_const(name: &syn::Ident) -> String {
    format!(
        "`{name}` can't be part of a constant value: it is a #[workflow] input, \
         a task result or the `fan_out` item, and a constant is fixed when the \
         workflow is emitted. Pass `{name}` on its own, or build the value in a \
         #[container] that takes `{name}` as an argument."
    )
}

/// Strict: a literal, a previous-task reference, a workflow-input
/// reference, or a constant. Anything else is a hard `compile_error!` (no
/// silent stringify): an unmodeled arg would otherwise emit a bogus Argo
/// param.
pub(crate) fn expr_to_arg(
    e: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
    item: Option<&str>,
) -> syn::Result<Arg> {
    let is_flow = |n: &str| bindings.contains_key(n) || inputs.contains(n) || item == Some(n);
    match unwrap_expr(e) {
        // Regime B: every param value is emitted as valid JSON (string
        // -> "v", int -> v, float -> v, bool -> true/false). The run
        // side already does `from_str` else `String`, so bodies are
        // unaffected; this makes the value unambiguous (fixes a `String`
        // "7" round-tripping as a number) and lets attribute
        // interpolation always use `{{=fromJSON(...)}}`.
        Expr::Lit(syn::ExprLit { lit, .. }) => Ok(match lit {
            syn::Lit::Str(s) => {
                Arg::Lit(serde_json::to_string(&s.value()).expect("string is JSON-serializable"))
            }
            // `base10_digits()` is already a valid JSON number.
            syn::Lit::Int(i) => Arg::Lit(i.base10_digits().to_string()),
            syn::Lit::Float(f) => Arg::Lit(f.base10_digits().to_string()),
            syn::Lit::Bool(b) => Arg::Lit(b.value.to_string()),
            // A char param is JSON-encoded as its one-char string (the
            // run side `from_str::<char>`s it back). The old catch-all
            // stringified the *token* — `step('a')` shipped `"'a'"`,
            // quotes included, and failed deserialization in-pod.
            syn::Lit::Char(c) => Arg::Lit(
                serde_json::to_string(&c.value().to_string()).expect("char is JSON-serializable"),
            ),
            _ => {
                return Err(syn::Error::new_spanned(
                    e,
                    "unsupported literal kind for a template argument \
                     (byte / byte-string / C-string literals have no JSON \
                     parameter encoding)",
                ));
            }
        }),
        // Type-preserving conversions of a literal stay macro-time.
        Expr::MethodCall(mc) if mc.args.is_empty() && lit_conversion(mc) => {
            expr_to_arg(&mc.receiver, bindings, inputs, item)
        }
        // Only top-level task args lower a nested call (`push_call`).
        Expr::Call(_) if call_parts(e).is_some() => Err(syn::Error::new_spanned(e, NESTED_CALL)),
        c if !flow_root(c).is_some_and(|n| is_flow(&n)) => Ok(Arg::Const(constant(c, &is_flow)?)),
        // Owned-value conversions, emitted identically to the receiver:
        //  * `.clone()`/`.to_owned()` are type-preserving. `.clone()` is
        //    the explicit fan-out marker (Argo copies the output param
        //    into each consumer); it vanishes in the emit.
        //  * `.to_string()`/`.into()` can CHANGE the type, but the emit
        //    only ever passes the receiver's raw serialized param: on a
        //    binding/input that would mismatch the ghost's type.
        Expr::MethodCall(mc) if mc.args.is_empty() => match mc.method.to_string().as_str() {
            "clone" | "to_owned" => expr_to_arg(&mc.receiver, bindings, inputs, item),
            "to_string" | "into" => Err(syn::Error::new_spanned(
                mc,
                "`.to_string()`/`.into()` on a #[workflow] input, a task result or \
                 the `fan_out` item isn't lowered: pass it directly, or `.clone()` \
                 to fan out.",
            )),
            _ => Err(syn::Error::new_spanned(mc, UNSUPPORTED_ARG)),
        },
        Expr::Path(p) => {
            let name = p.path.segments[0].ident.to_string();
            if let Some(task) = bindings.get(&name) {
                Ok(Arg::Ref(task.clone()))
            } else if inputs.contains(&name) {
                Ok(Arg::Input(name))
            } else {
                Err(syn::Error::new_spanned(e, UNSUPPORTED_ARG))
            }
        }
        // `a.b.c` — named-field access into a serde value. The base must
        // resolve to a binding/input; the ghost has already type-checked
        // that the field path is valid on the producer's real type.
        Expr::Field(_) => {
            let mut path: Vec<String> = Vec::new();
            let mut cur = unwrap_expr(e);
            while let Expr::Field(fe) = cur {
                match &fe.member {
                    syn::Member::Named(id) => path.push(id.to_string()),
                    syn::Member::Unnamed(_) => {
                        return Err(syn::Error::new_spanned(
                            fe,
                            "tuple-field access (`a.0`) isn't lowered yet — \
                             v1 supports named struct fields (`a.b.c`).",
                        ));
                    }
                }
                cur = unwrap_expr(&fe.base);
            }
            path.reverse();
            let Expr::Path(p) = cur else {
                return Err(syn::Error::new_spanned(
                    cur,
                    "field-access base must be a #[workflow] input or a \
                     prior `let = template(...)` binding.",
                ));
            };
            let name = p.path.segments[0].ident.to_string();
            let src = if let Some(task) = bindings.get(&name) {
                JsonSrc::Task(task.clone())
            } else if inputs.contains(&name) {
                JsonSrc::Input(name)
            } else {
                return Err(syn::Error::new_spanned(cur, UNSUPPORTED_ARG));
            };
            Ok(Arg::Json { src, path })
        }
        Expr::Index(idx) => Err(syn::Error::new_spanned(
            idx,
            "index access (`a[i]`) isn't lowered yet — v1 supports named \
             struct fields (`a.b.c`).",
        )),
        other => Err(syn::Error::new_spanned(other, UNSUPPORTED_ARG)),
    }
}

/// `lit.clone()` / `lit.to_owned()`, or `"s".to_string()`.
fn lit_conversion(mc: &syn::ExprMethodCall) -> bool {
    match mc.method.to_string().as_str() {
        "clone" | "to_owned" => matches!(unwrap_expr(&mc.receiver), Expr::Lit(_)),
        "to_string" => is_str_lit(&mc.receiver),
        _ => false,
    }
}

fn is_str_lit(e: &Expr) -> bool {
    matches!(
        unwrap_expr(e),
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(_),
            ..
        })
    )
}

/// Single-ident root of a field / index / method / unary chain.
fn flow_root(e: &Expr) -> Option<String> {
    match unwrap_expr(e) {
        Expr::Field(f) => flow_root(&f.base),
        Expr::Index(i) => flow_root(&i.expr),
        Expr::MethodCall(m) => flow_root(&m.receiver),
        Expr::Unary(u) => flow_root(&u.expr),
        Expr::Path(p) if p.qself.is_none() => p.path.get_ident().map(|i| i.to_string()),
        _ => None,
    }
}

/// Validate a constant and seal it for emit: each value path becomes
/// `const { P }`, so rustc rejects a non-`Copy` static.
fn constant(e: &Expr, is_flow: &dyn Fn(&str) -> bool) -> syn::Result<Expr> {
    let mut out = e.clone();
    match &mut out {
        Expr::Lit(_) | Expr::Const(_) => {}
        Expr::Path(p) => {
            if p.qself.is_none()
                && let Some(id) = p.path.get_ident()
                && is_flow(&id.to_string())
            {
                return Err(syn::Error::new_spanned(&*p, mixed_const(id)));
            }
            return Ok(syn::parse_quote_spanned!(p.span()=> const { #p }));
        }
        Expr::Struct(s) => {
            for f in &mut s.fields {
                f.expr = constant(&f.expr, is_flow)?;
                // Shorthand `Opts { n }` must print as `n: <value>`.
                f.colon_token.get_or_insert_with(Default::default);
            }
            if let Some(r) = &mut s.rest {
                **r = constant(r, is_flow)?;
            }
        }
        Expr::Tuple(syn::ExprTuple { elems, .. }) | Expr::Array(syn::ExprArray { elems, .. }) => {
            for x in elems {
                *x = constant(x, is_flow)?;
            }
        }
        Expr::Call(c) => {
            if !matches!(&*c.func, Expr::Path(f) if f.qself.is_none() && is_ctor(&f.path)) {
                return Err(syn::Error::new_spanned(&*c, CONST_CALL));
            }
            for x in &mut c.args {
                *x = constant(x, is_flow)?;
            }
        }
        Expr::MethodCall(m) if m.args.is_empty() && m.turbofish.is_none() => {
            match m.method.to_string().as_str() {
                "clone" | "to_owned" | "to_string" | "to_vec" => {
                    *m.receiver = constant(&m.receiver, is_flow)?;
                }
                "into" if is_str_lit(&m.receiver) => {}
                "into" => return Err(syn::Error::new_spanned(&*m, CONST_INTO)),
                _ => return Err(syn::Error::new_spanned(&*m, UNSUPPORTED_ARG)),
            }
        }
        Expr::Field(f) => *f.base = constant(&f.base, is_flow)?,
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => {
            *u.expr = constant(&u.expr, is_flow)?;
        }
        Expr::Paren(syn::ExprParen { expr, .. }) | Expr::Group(syn::ExprGroup { expr, .. }) => {
            **expr = constant(expr, is_flow)?;
        }
        other => return Err(syn::Error::new_spanned(&*other, UNSUPPORTED_ARG)),
    }
    Ok(out)
}

/// Like `expr_to_arg`, but inside a `fan_out` closure `|it| …`: an
/// expression rooted at the item param `it` (optionally `.clone()`/
/// `.to_owned()`-wrapped, with named `.field`s) becomes `Arg::Item`
/// (`{{item}}`/`{{item.f}}`); anything else resolves normally.
pub(crate) fn resolve_arg(
    e: &Expr,
    item: Option<&str>,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<Arg> {
    if let Some(it) = item {
        let mut cur = unwrap_expr(e);
        while let Expr::MethodCall(mc) = cur {
            if mc.args.is_empty() && matches!(mc.method.to_string().as_str(), "clone" | "to_owned")
            {
                cur = unwrap_expr(&mc.receiver);
            } else {
                break;
            }
        }
        let mut path: Vec<String> = Vec::new();
        while let Expr::Field(fe) = cur {
            match &fe.member {
                syn::Member::Named(id) => path.push(id.to_string()),
                syn::Member::Unnamed(_) => {
                    return Err(syn::Error::new_spanned(
                        fe,
                        "tuple-field access on a `fan_out` item isn't \
                         lowered yet — use named fields.",
                    ));
                }
            }
            cur = unwrap_expr(&fe.base);
        }
        if let Expr::Path(p) = cur
            && p.path.is_ident(it)
        {
            path.reverse();
            return Ok(Arg::Item { path });
        }
    }
    expr_to_arg(e, bindings, inputs, item)
}

/// The tail expression of a closure body (`|x| C(x)` or `|x| { …; C(x) }`).
pub(crate) fn closure_tail(b: &Expr) -> &Expr {
    if let Expr::Block(eb) = b
        && let Some(Stmt::Expr(e, _)) = eb.block.stmts.last()
    {
        return e;
    }
    b
}

/// `<list>.fan_out(|item| C(args))` → `(receiver, item-name, C, C-args)`.
/// `None` if `e` isn't that exact shape.
pub(crate) fn fan_parts(e: &Expr) -> Option<(Expr, String, Path, Vec<Expr>)> {
    let Expr::MethodCall(mc) = unwrap_expr(e) else {
        return None;
    };
    if mc.method != "fan_out" {
        return None;
    }
    let mut it = mc.args.iter();
    let (Some(a0), None) = (it.next(), it.next()) else {
        return None;
    };
    let Expr::Closure(cl) = unwrap_expr(a0) else {
        return None;
    };
    if cl.inputs.len() != 1 {
        return None;
    }
    let item = match local_binding(&cl.inputs[0]) {
        Ok(Some(n)) => n,
        _ => return None,
    };
    let (callee, raw) = call_parts(closure_tail(&cl.body))?;
    Some(((*mc.receiver).clone(), item, callee, raw))
}

/// Resolve a `fan_out` receiver (the list) to its Argo source: a prior
/// `let` binding (a producing task, adds a dep) or a `#[workflow]` input.
pub(crate) fn fan_src(
    recv: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<FanSrc> {
    if let Expr::Path(p) = unwrap_expr(recv)
        && let Some(name) = p.path.get_ident().map(|i| i.to_string())
    {
        if let Some(task) = bindings.get(&name) {
            return Ok(FanSrc::Task(task.clone()));
        } else if inputs.contains(&name) {
            return Ok(FanSrc::Input(name));
        }
    }
    Err(syn::Error::new_spanned(
        recv,
        "`fan_out` source must be a task result (`let x = template(..);`) or \
         a #[workflow] input (a list), not a constant.",
    ))
}

/// A template call `t(args)`; a constructor call (`Some(3)`) is a constant.
pub(crate) fn call_parts(e: &Expr) -> Option<(Path, Vec<Expr>)> {
    if let Expr::Call(c) = unwrap_expr(e)
        && let Expr::Path(p) = &*c.func
        && !(p.qself.is_none() && is_ctor(&p.path))
    {
        return Some((p.path.clone(), c.args.iter().cloned().collect()));
    }
    None
}

/// A bare path expression → its `Path` (a template identity).
pub(crate) fn expr_path(e: &Expr) -> Option<Path> {
    match unwrap_expr(e) {
        Expr::Path(p) => Some(p.path.clone()),
        _ => None,
    }
}

/// A hook target: `t` (bare path) or `t(arg, …)` (call with args).
pub(crate) fn hook_target(arg: &Expr) -> syn::Result<(Path, Vec<Expr>)> {
    if let Some((p, raw)) = call_parts(arg) {
        Ok((p, raw))
    } else if let Some(p) = expr_path(arg) {
        Ok((p, Vec::new()))
    } else {
        Err(syn::Error::new_spanned(
            arg,
            "hook target must be a template path `t` or call `t(args)`.",
        ))
    }
}

/// The single template-target arg of `.on_exit/.on_success/.on_failure/
/// .on_error(t)` (exactly one).
pub(crate) fn single_hook_target(mc: &syn::ExprMethodCall) -> syn::Result<(Path, Vec<Expr>)> {
    let mut it = mc.args.iter();
    let (Some(arg), None) = (it.next(), it.next()) else {
        return Err(syn::Error::new_spanned(
            mc,
            "expected exactly one template: `.<hook>(t)` or \
             `.<hook>(t(args))`.",
        ));
    };
    hook_target(arg)
}

/// Peel trailing builder method calls (`.continue_on`/`.hooks`/`.on_exit`)
/// off `e`, accumulating a `NodeOpts`, and return the inner base
/// expression (which the caller still validates is a template call). An
/// unknown trailing method is *not* consumed — left for the caller's
/// normal not-a-template-call diagnostic — but a malformed *known*
/// builder is a hard, targeted `compile_error!`.
pub(crate) fn peel_builders(e: &Expr) -> syn::Result<(&Expr, NodeOpts)> {
    let mut opts = NodeOpts::default();
    let mut on_exit_seen = false;
    let mut cur = e;
    while let Expr::MethodCall(mc) = unwrap_expr(cur) {
        match mc.method.to_string().as_str() {
            "continue_on" => {
                if opts.continue_on.is_some() {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.continue_on(...)` specified more than once.",
                    ));
                }
                if mc.args.is_empty() {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.continue_on(...)` needs `failed` and/or `error`.",
                    ));
                }
                let (mut err, mut failed) = (false, false);
                for a in &mc.args {
                    match expr_path(a).and_then(|p| p.get_ident().map(|i| i.to_string())) {
                        Some(s) if s == "error" => err = true,
                        Some(s) if s == "failed" => failed = true,
                        _ => {
                            return Err(syn::Error::new_spanned(
                                a,
                                "`.continue_on(...)` only accepts the bare \
                                 idents `failed` and/or `error`.",
                            ));
                        }
                    }
                }
                opts.continue_on = Some((err, failed));
            }
            "on_exit" => {
                if on_exit_seen {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.on_exit(...)` specified more than once.",
                    ));
                }
                on_exit_seen = true;
                let (template, raw_args) = single_hook_target(mc)?;
                opts.hooks.push(HookSpec {
                    when: HookWhen::Exit,
                    template,
                    raw_args,
                });
            }
            // Typed phase predicates — athena generates the Argo
            // `expression`. Repeatable (each = a distinct auto-keyed hook).
            m @ ("on_success" | "on_failure" | "on_error") => {
                let when = match m {
                    "on_success" => HookWhen::Success,
                    "on_failure" => HookWhen::Failure,
                    _ => HookWhen::Error,
                };
                let (template, raw_args) = single_hook_target(mc)?;
                opts.hooks.push(HookSpec {
                    when,
                    template,
                    raw_args,
                });
            }
            // Escape hatch: raw Argo expression(s) -> template(args).
            "hook_if" => {
                if mc.args.is_empty() {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.hook_if(...)` needs at least one \
                         `\"argo-expression\" = template` entry.",
                    ));
                }
                for a in &mc.args {
                    let Expr::Assign(asn) = unwrap_expr(a) else {
                        return Err(syn::Error::new_spanned(
                            a,
                            "each `.hook_if(...)` entry must be \
                             `\"argo-expression\" = template`.",
                        ));
                    };
                    let expression = match unwrap_expr(&asn.left) {
                        Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s),
                            ..
                        }) => s.value(),
                        _ => {
                            return Err(syn::Error::new_spanned(
                                &asn.left,
                                "`.hook_if(...)` key must be a string-literal \
                                 Argo expression.",
                            ));
                        }
                    };
                    let (template, raw_args) = hook_target(&asn.right)?;
                    opts.hooks.push(HookSpec {
                        when: HookWhen::Raw(expression),
                        template,
                        raw_args,
                    });
                }
            }
            // Unknown trailing method: stop peeling. The caller's
            // call_parts() will fail on it and emit the usual
            // not-a-template-call / unsupported-statement error.
            _ => break,
        }
        cur = &mc.receiver;
    }
    // We traverse outermost→innermost = reverse source order; restore it
    // so hook keys (hook1, hook2, …) are deterministic & source-ordered.
    opts.hooks.reverse();
    Ok((cur, opts))
}

/// Short name for a callee path (its last segment), used to derive a
/// default DAG task name.
pub(crate) fn path_leaf(p: &Path) -> String {
    p.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_else(|| "task".to_string())
}
/// Catch-all kept for the few sites that pass an `Expr` we can't or
/// don't bother narrowing (returned-binding paths, etc.). Prefer
/// [`unsupported_stmt_msg`] when you have an `Expr` in hand.
pub(crate) const UNSUPPORTED_STMT: &str = "unsupported statement in a #[workflow]. \
Only `let x = template(args);`, `let x = <constant>;`, `template(args);`, and \
`if`/`else if`/`else` are lowered. Anything else is rejected so a step is \
never silently dropped.";

/// Tailored advice per statement shape. Steers the user toward the
/// feature that *does* model what they probably wanted (fan_out for
/// loops, if/else for match, the builder chain for method calls), so
/// the error doubles as a hint.
pub(crate) fn unsupported_stmt_msg(expr: &Expr) -> String {
    use Expr::*;
    match expr {
        ForLoop(_) => "`for` loops aren't lowered. For per-element parallel work use \
            `list.fan_out(|x| step(x))`; for sequential work, thread a return \
            value through to make the dependency explicit."
            .to_string(),
        While(_) | Loop(_) => "`while`/`loop` aren't lowered. A #[workflow] body is read \
            once to build the DAG, not iterated at runtime. Move the loop \
            inside a #[container] body, or use `.fan_out` for parallelism."
            .to_string(),
        Match(_) => "`match` isn't lowered yet. For exclusive branches in a #[workflow], \
            use `if` / `else if` / `else` (supported)."
            .to_string(),
        MethodCall(mc) => format!(
            "`.{m}(..)` isn't a supported chain in a #[workflow]. \
             The lowered chains are `.clone()`/`.to_owned()` on args, \
             `.fan_out(|x| C(x, ..))`, `.continue_on(..)`, \
             `.on_success(..)`/`.on_failure(..)`/`.on_error(..)`/\
             `.on_exit(..)`/`.hook_if(..)`.",
            m = mc.method
        ),
        Try(_) => "`?` (the Try operator) isn't supported in a #[workflow] body. \
            Templates can't propagate errors via Rust's Try; use \
            `.continue_on(failed, error)` to let dependents proceed when a \
            step fails."
            .to_string(),
        Async(_) | Await(_) => "`async`/`await` isn't supported in a #[workflow] body. \
            The workflow is statically analyzed at compile time; the steps \
            run in pods, not here. (For async work *inside* a container, \
            `#[container] async fn` works with the `tokio` feature.)"
            .to_string(),
        Closure(_) => "a bare closure isn't a statement in a #[workflow]. The only \
            place a closure is accepted is inside `.fan_out(|x| C(x, ..))`."
            .to_string(),
        Block(_) => "a bare block isn't a statement in a #[workflow]. Each step \
            must be a single template call; group with a sub-#[workflow] \
            if you want reusable composition."
            .to_string(),
        Unsafe(_) => "`unsafe` blocks aren't a workflow statement. A #[workflow] \
            body only describes the DAG; put any unsafe code inside a \
            #[container] body (it runs there)."
            .to_string(),
        _ => UNSUPPORTED_STMT.to_string(),
    }
}

pub(crate) const NOT_A_TEMPLATE_CALL: &str = "expected a template call `name(args)`. \
Only #[container] and #[workflow] are templates - #[fragment]s and regular \
functions can't be called from a #[workflow] body.";

pub(crate) const FAN_OUT_BODY_NOT_A_CALL: &str = "a `.fan_out(|x| …)` closure body must \
be a single template call like `step(x)` or `step(x, lit)`. The closure \
runs once per element; its return value joins the aggregated `Vec`.";

/// Detect `<recv>.fan_out(...)` with a closure body that *isn't* a
/// template call. Lets the error point at the actual problem (the
/// closure) instead of saying \"this whole expression isn't a call\".
pub(crate) fn fan_out_bad_closure(e: &Expr) -> Option<&Expr> {
    let Expr::MethodCall(mc) = unwrap_expr(e) else {
        return None;
    };
    if mc.method != "fan_out" || mc.args.len() != 1 {
        return None;
    }
    let Expr::Closure(cl) = unwrap_expr(&mc.args[0]) else {
        return None;
    };
    // If it parses as a fan_out shape (one ident arg + a body), but the
    // body isn't a call, we want to error specifically about the body.
    if cl.inputs.len() != 1 {
        return None;
    }
    let body = closure_tail(&cl.body);
    if call_parts(body).is_some() {
        return None;
    }
    Some(body)
}

pub(crate) const MACRO_IN_WORKFLOW: &str = "macros aren't lowered to workflow steps. \
A macro call here would be dropped from the DAG. If you need pod resources \
(`host!`, `secret!`, `load_artifact!`, `save_artifact!`), declare them \
inside a #[container] body; only container bodies actually run.";

pub(crate) const RETURN_UNRESOLVED: &str = "this #[workflow] declares a return type but \
its returned value isn't produced by a template call. End with a tail \
template call `name(args)` (no `;`) or return a `let` binding from one.";

/// The binding name a `let` pattern introduces: `Some(name)` for a plain
/// (optionally `mut`/typed) ident, `None` for `_`. Anything else (tuple,
/// ref, struct, or-pattern) is unsupported in a #[workflow].
pub(crate) fn local_binding(pat: &syn::Pat) -> syn::Result<Option<String>> {
    match pat {
        syn::Pat::Ident(p) if p.by_ref.is_none() && p.subpat.is_none() => {
            Ok(Some(p.ident.to_string()))
        }
        syn::Pat::Wild(_) => Ok(None),
        syn::Pat::Type(t) => local_binding(&t.pat),
        other => Err(syn::Error::new_spanned(
            other,
            "unsupported `let` pattern in #[workflow]: bind a single name \
             (`let x = template(...);`) or `_`.",
        )),
    }
}

/// expr-lang reserved words. Argo evaluates `depends` with bare task
/// names (`-` -> `_`), so a task named one of these breaks its dependents.
const EXPR_RESERVED: &[&str] = &[
    "not", "in", "or", "and", "matches", "contains", "let", "if", "else", "nil", "true", "false",
];

/// Allocate a kebab task name unique within the workflow.
pub(crate) fn uniq_task(used: &mut std::collections::HashSet<String>, base: &str) -> String {
    let mut task = kebab(base);
    let mut n = 1;
    while used.contains(&task) || EXPR_RESERVED.contains(&task.as_str()) {
        n += 1;
        task = format!("{}-{n}", kebab(base));
    }
    used.insert(task.clone());
    task
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn push_call(
    callee: Path,
    raw: Vec<Expr>,
    base: &str,
    opts: NodeOpts,
    item: Option<&str>,
    fan: Option<FanSrc>,
    used: &mut std::collections::HashSet<String>,
    nodes: &mut Vec<Node>,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<String> {
    let task = uniq_task(used, base);
    // A template *call* in argument position (`foo(bar())`) is lowered
    // to its own task; `foo` then takes a ref to it (a DAG dep), exactly
    // like a prior `let`. Recursive (`foo(bar(baz()))`). Not applied
    // inside a `fan_out` closure (the item scope) in v1.
    let mut args: Vec<Arg> = Vec::with_capacity(raw.len());
    for a in &raw {
        if item.is_none()
            && let Some((c2, r2)) = call_parts(a)
        {
            let leaf = path_leaf(&c2);
            let t = push_call(
                c2,
                r2,
                &leaf,
                NodeOpts::default(),
                None,
                None,
                used,
                nodes,
                bindings,
                inputs,
            )?;
            args.push(Arg::Ref(t));
        } else {
            args.push(resolve_arg(a, item, bindings, inputs)?);
        }
    }
    let hooks = opts
        .hooks
        .into_iter()
        .map(|h| {
            let args = h
                .raw_args
                .iter()
                .map(|a| resolve_arg(a, item, bindings, inputs))
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(Hook {
                when: h.when,
                template: h.template,
                args,
            })
        })
        .collect::<syn::Result<Vec<_>>>()?;
    nodes.push(Node {
        task: task.clone(),
        callee,
        args,
        continue_on: opts.continue_on,
        hooks,
        fan,
        when: None,
    });
    Ok(task)
}

/// Distinct callee + hook-template paths in `nodes` (for `collect`).
pub(crate) fn callee_paths(nodes: &[Node], extra: &[Path]) -> Vec<Path> {
    let mut seen = std::collections::HashSet::new();
    nodes
        .iter()
        .flat_map(|n| std::iter::once(&n.callee).chain(n.hooks.iter().map(|h| &h.template)))
        .chain(extra.iter())
        .filter(|p| seen.insert(quote!(#p).to_string()))
        .cloned()
        .collect()
}

/// A `let` init that lowers to a task (else it's a constant).
fn is_step(e: &Expr) -> bool {
    matches!(unwrap_expr(e), Expr::MethodCall(mc)
        if mc.method == "fan_out" || is_builder_method(&mc.method))
        || call_parts(e).is_some()
}

/// Inlines constant `let` bindings: a use of a bound name becomes its
/// value, located at the use. A nested `let` or closure param shadows,
/// as in Rust.
struct Inline(std::collections::HashMap<String, Expr>);

impl VisitMut for Inline {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        if let Expr::Path(p) = e
            && p.qself.is_none()
            && let Some(v) = p.path.get_ident().and_then(|i| self.0.get(&i.to_string()))
        {
            *e = located_at(v, p.span());
        } else if let Expr::Closure(c) = e {
            let outer = self.0.clone();
            for p in &c.inputs {
                if let Ok(Some(n)) = local_binding(p) {
                    self.0.remove(&n);
                }
            }
            self.visit_expr_mut(&mut c.body);
            self.0 = outer;
        } else if let Expr::MethodCall(m) = e
            && m.method == "continue_on"
        {
            // Its args are keywords (`failed`), not values.
            self.visit_expr_mut(&mut m.receiver);
        } else {
            syn::visit_mut::visit_expr_mut(self, e);
        }
    }

    fn visit_block_mut(&mut self, b: &mut syn::Block) {
        let outer = self.0.clone();
        for s in &mut b.stmts {
            self.visit_stmt_mut(s);
            if let Stmt::Local(l) = s
                && let Ok(Some(n)) = local_binding(&l.pat)
            {
                self.0.remove(&n);
            }
        }
        self.0 = outer;
    }

    // Shorthand `Opts { a }` must print as `a: <value>`.
    fn visit_field_value_mut(&mut self, f: &mut syn::FieldValue) {
        syn::visit_mut::visit_field_value_mut(self, f);
        f.colon_token.get_or_insert_with(Default::default);
    }

    // A returned name stays: "`k` is returned but isn't a binding".
    fn visit_stmt_mut(&mut self, s: &mut Stmt) {
        if !matches!(s, Stmt::Expr(Expr::Path(_), _)) {
            syn::visit_mut::visit_stmt_mut(self, s);
        }
    }

    fn visit_expr_return_mut(&mut self, r: &mut syn::ExprReturn) {
        if !matches!(r.expr.as_deref(), Some(Expr::Path(_))) {
            syn::visit_mut::visit_expr_return_mut(self, r);
        }
    }
}

/// `e` with every token located at `at` (hygiene kept).
fn located_at(e: &Expr, at: Span) -> Expr {
    fn walk(ts: TokenStream2, at: Span) -> TokenStream2 {
        ts.into_iter()
            .map(|mut t| {
                if let TokenTree::Group(g) = &t {
                    let mut n = Group::new(g.delimiter(), walk(g.stream(), at));
                    n.set_span(g.span().located_at(at));
                    t = TokenTree::Group(n);
                } else {
                    t.set_span(t.span().located_at(at));
                }
                t
            })
            .collect()
    }
    syn::parse2(walk(quote!(#e), at)).expect("an inlined constant re-parses")
}

pub(crate) fn analyze_stmts(
    stmts: &[Stmt],
    inputs: &std::collections::HashSet<String>,
    want_output: bool,
    argo_self: &str,
    rust_self: &str,
    ctx: &mut SynthCtx,
) -> syn::Result<(Vec<Node>, Option<String>)> {
    let mut bindings: std::collections::HashMap<String, String> = Default::default();
    let mut consts: std::collections::HashMap<String, Expr> = Default::default();
    let mut used: std::collections::HashSet<String> = Default::default();
    let mut nodes: Vec<Node> = Vec::new();
    let mut output_task: Option<String> = None;

    // Recurse with this slice's identity as the synth parent name.
    let saved = (ctx.parent_rust.clone(), ctx.parent_argo.clone());
    ctx.parent_rust = rust_self.to_string();
    ctx.parent_argo = argo_self.to_string();

    for (idx, stmt) in stmts.iter().enumerate() {
        let is_last = idx + 1 == stmts.len();
        let inlined;
        let stmt = if consts.is_empty() {
            stmt
        } else {
            let mut s = stmt.clone();
            Inline(consts.clone()).visit_stmt_mut(&mut s);
            inlined = s;
            &inlined
        };
        match stmt {
            Stmt::Local(local) => {
                let bind = local_binding(&local.pat)?;
                let init = local
                    .init
                    .as_ref()
                    .ok_or_else(|| syn::Error::new_spanned(local, LET_SHAPE))?;
                if init.diverge.is_some() {
                    return Err(syn::Error::new_spanned(
                        local,
                        "`let ... else` is not supported in #[workflow].",
                    ));
                }
                let task = if let Expr::If(ei) = unwrap_expr(&init.expr) {
                    synth_if(ei, true, &bindings, inputs, &mut used, &mut nodes, ctx)?
                } else if !is_step(&init.expr) {
                    // A constant `let`: no task, inlined at each use.
                    if !matches!(
                        expr_to_arg(&init.expr, &bindings, inputs, None)?,
                        Arg::Lit(_) | Arg::Const(_)
                    ) {
                        return Err(syn::Error::new_spanned(&init.expr, LET_SHAPE));
                    }
                    // Inlining drops the annotation, so emit would infer
                    // the value's type on its own.
                    if let syn::Pat::Type(t) = &local.pat {
                        return Err(syn::Error::new_spanned(&t.ty, CONST_LET_TYPE));
                    }
                    if let Some(b) = bind {
                        bindings.remove(&b);
                        consts.insert(b, (*init.expr).clone());
                    }
                    continue;
                } else {
                    let (base_expr, opts) = peel_builders(&init.expr)?;
                    if let Some((recv, item, callee, raw)) = fan_parts(base_expr) {
                        let fsrc = fan_src(&recv, &bindings, inputs)?;
                        let base = path_leaf(&callee);
                        push_call(
                            callee,
                            raw,
                            &base,
                            opts,
                            Some(item.as_str()),
                            Some(fsrc),
                            &mut used,
                            &mut nodes,
                            &bindings,
                            inputs,
                        )?
                    } else {
                        let (callee, raw) = call_parts(base_expr).ok_or_else(|| {
                            if let Some(body) = fan_out_bad_closure(base_expr) {
                                syn::Error::new_spanned(body, FAN_OUT_BODY_NOT_A_CALL)
                            } else {
                                syn::Error::new_spanned(&init.expr, NOT_A_TEMPLATE_CALL)
                            }
                        })?;
                        let base = path_leaf(&callee);
                        push_call(
                            callee, raw, &base, opts, None, None, &mut used, &mut nodes, &bindings,
                            inputs,
                        )?
                    }
                };
                if let Some(b) = bind {
                    consts.remove(&b);
                    bindings.insert(b, task);
                }
            }
            Stmt::Expr(expr, semi) => {
                if let Expr::If(ei) = unwrap_expr(expr) {
                    let value = is_last && semi.is_none() && want_output;
                    let task = synth_if(ei, value, &bindings, inputs, &mut used, &mut nodes, ctx)?;
                    if value {
                        output_task = Some(task);
                    }
                } else if let Expr::Return(r) = unwrap_expr(expr) {
                    // A workflow lowers to a DAG, not control flow:
                    // statements after a `return` would still be lowered
                    // as tasks that RUN in Argo, and a later `return`
                    // would win (the opposite of Rust). Only the final
                    // statement may be a `return`.
                    if !is_last {
                        return Err(syn::Error::new_spanned(
                            expr,
                            "`return` must be the last statement of a #[workflow] \
                             body — later statements would still run as DAG tasks \
                             in Argo (nothing is unreachable in a compiled workflow)",
                        ));
                    }
                    let target = r.expr.as_deref().ok_or_else(|| {
                        syn::Error::new_spanned(
                            expr,
                            "#[workflow] `return` must return a template result.",
                        )
                    })?;
                    if let Expr::If(ei) = unwrap_expr(target) {
                        output_task = Some(synth_if(
                            ei, true, &bindings, inputs, &mut used, &mut nodes, ctx,
                        )?);
                    } else {
                        let (base_expr, opts) = peel_builders(target)?;
                        output_task = Some(match unwrap_expr(base_expr) {
                            Expr::Path(p) if p.path.segments.len() == 1 => {
                                if opts.continue_on.is_some() || !opts.hooks.is_empty() {
                                    return Err(syn::Error::new_spanned(
                                        target,
                                        "`.continue_on`/`.hooks`/`.on_exit` \
                                         must be chained on a template \
                                         call, not a returned binding.",
                                    ));
                                }
                                let name = p.path.segments[0].ident.to_string();
                                bindings.get(&name).cloned().ok_or_else(|| {
                                    syn::Error::new_spanned(
                                        target,
                                        format!(
                                            "`{name}` is returned but \
                                                 isn't a binding from a \
                                                 `let = template(...)`."
                                        ),
                                    )
                                })?
                            }
                            _ => {
                                let (callee, raw) = call_parts(base_expr).ok_or_else(|| {
                                    if let Some(body) = fan_out_bad_closure(base_expr) {
                                        syn::Error::new_spanned(body, FAN_OUT_BODY_NOT_A_CALL)
                                    } else {
                                        syn::Error::new_spanned(target, NOT_A_TEMPLATE_CALL)
                                    }
                                })?;
                                let base = path_leaf(&callee);
                                push_call(
                                    callee, raw, &base, opts, None, None, &mut used, &mut nodes,
                                    &bindings, inputs,
                                )?
                            }
                        });
                    }
                } else if let Expr::Path(p) = unwrap_expr(expr) {
                    if !(is_last && semi.is_none() && p.path.segments.len() == 1) {
                        return Err(syn::Error::new_spanned(expr, unsupported_stmt_msg(expr)));
                    }
                    let name = p.path.segments[0].ident.to_string();
                    output_task = Some(bindings.get(&name).cloned().ok_or_else(|| {
                        syn::Error::new_spanned(
                            expr,
                            format!(
                                "`{name}` is returned but isn't a \
                                     binding from a `let = template(...)`."
                            ),
                        )
                    })?);
                } else {
                    let (base_expr, opts) = peel_builders(expr)?;
                    let task = if let Some((recv, item, callee, raw)) = fan_parts(base_expr) {
                        let fsrc = fan_src(&recv, &bindings, inputs)?;
                        let base = path_leaf(&callee);
                        push_call(
                            callee,
                            raw,
                            &base,
                            opts,
                            Some(item.as_str()),
                            Some(fsrc),
                            &mut used,
                            &mut nodes,
                            &bindings,
                            inputs,
                        )?
                    } else {
                        let (callee, raw) = call_parts(base_expr).ok_or_else(|| {
                            syn::Error::new_spanned(expr, unsupported_stmt_msg(expr))
                        })?;
                        let base = path_leaf(&callee);
                        push_call(
                            callee, raw, &base, opts, None, None, &mut used, &mut nodes, &bindings,
                            inputs,
                        )?
                    };
                    if is_last && semi.is_none() && want_output {
                        output_task = Some(task);
                    }
                }
            }
            Stmt::Macro(m) => {
                return Err(syn::Error::new_spanned(m, MACRO_IN_WORKFLOW));
            }
            Stmt::Item(it) => {
                return Err(syn::Error::new_spanned(
                    it,
                    "nested items (fn / mod / struct / enum / const / static / \
                     use / etc.) aren't lowered in a #[workflow] body. Move \
                     them to the surrounding module.",
                ));
            }
        }
    }

    ctx.parent_rust = saved.0;
    ctx.parent_argo = saved.1;

    // NOTE: an unresolved output (`want_output` but no terminal task) is
    // NOT an error here — each caller reports [`RETURN_UNRESOLVED`] with
    // its own precise span (the fn's return type at top level, the arm
    // body inside a value-`if`). Signalling it through the return value
    // instead of matching on error TEXT keeps arm-local spans intact.
    Ok((nodes, if want_output { output_task } else { None }))
}

/// Top-level: analyze a `#[workflow]` body, returning its nodes, terminal
/// output task, and every synthesized `if` wrapper/arm to also emit.
pub(crate) fn analyze_workflow(
    func: &ItemFn,
    parent_argo: &str,
) -> syn::Result<(Vec<Node>, Option<String>, Vec<SynthWf>)> {
    let inputs: std::collections::HashSet<String> =
        fn_args(func).iter().map(|(i, _)| i.to_string()).collect();
    let want_output = matches!(func.sig.output, syn::ReturnType::Type(..));
    let mut ctx = SynthCtx {
        synth: Vec::new(),
        if_ctr: 0,
        parent_rust: func.sig.ident.to_string(),
        parent_argo: parent_argo.to_string(),
    };
    let (mut nodes, output_task) = analyze_stmts(
        &func.block.stmts,
        &inputs,
        want_output,
        parent_argo,
        &func.sig.ident.to_string(),
        &mut ctx,
    )?;
    if want_output && output_task.is_none() {
        return Err(syn::Error::new_spanned(&func.sig.output, RETURN_UNRESOLVED));
    }
    let (fan_tasks, fallible_tasks) = retag_refs(&mut nodes);
    // Consumable but not returnable (arm bodies get the same checks in
    // `synth_if`): a fan aggregate would bubble double-encoded, a
    // fallible binding has no `Result` bubble encoding.
    if let Some(t) = &output_task {
        if fan_tasks.contains(t) {
            return Err(syn::Error::new_spanned(
                &func.sig.output,
                FAN_RETURN_UNSUPPORTED,
            ));
        }
        if fallible_tasks.contains(t) {
            return Err(syn::Error::new_spanned(
                &func.sig.output,
                FALLIBLE_RETURN_UNSUPPORTED,
            ));
        }
    }
    Ok((nodes, output_task, ctx.synth))
}

pub(crate) const FAN_RETURN_UNSUPPORTED: &str = "a `fan_out` binding cannot be returned as the workflow's value (the raw \
Argo aggregate's elements are individually JSON-encoded, so the parent would \
read a double-encoded array). Pass the binding to a consuming template inside \
this workflow and return that instead.";

pub(crate) const FALLIBLE_RETURN_UNSUPPORTED: &str = "a `.continue_on(..)` binding is a `Result<T, ArgoError>` and can't be \
returned as the workflow's value. Pass it to a consuming template inside \
this workflow and return that instead.";

/// Per-scope arg re-tag (task and hook args alike): a `Ref` to a
/// `fan_out` task becomes an aggregate, a `Ref` to a `.continue_on`
/// task becomes `Result`-encoded. Scope-local: run once per scope
/// (top-level body AND every `if`-arm body). Returns (fan tasks,
/// fallible tasks) so callers can reject either as the scope's
/// terminal output.
pub(crate) fn retag_refs(
    nodes: &mut [Node],
) -> (
    std::collections::HashSet<String>,
    std::collections::HashSet<String>,
) {
    let fan_srcs: std::collections::HashMap<String, FanSrc> = nodes
        .iter()
        .filter_map(|n| n.fan.clone().map(|f| (n.task.clone(), f)))
        .collect();
    let fan_tasks: std::collections::HashSet<String> = fan_srcs.keys().cloned().collect();
    let fallible: std::collections::HashSet<String> = nodes
        .iter()
        .filter(|n| n.continue_on.is_some())
        .map(|n| n.task.clone())
        .collect();
    for n in nodes.iter_mut() {
        let args = n
            .args
            .iter_mut()
            .chain(n.hooks.iter_mut().flat_map(|h| h.args.iter_mut()));
        for a in args {
            if let Arg::Ref(t) = a {
                *a = match (fan_srcs.get(t), fallible.contains(t)) {
                    (Some(src), true) => Arg::FanAggFallible {
                        task: t.clone(),
                        src: src.clone(),
                    },
                    (Some(_), false) => Arg::FanAgg(t.clone()),
                    (None, true) => Arg::RefFallible(t.clone()),
                    (None, false) => continue,
                };
            }
        }
    }
    (fan_tasks, fallible)
}

#[cfg(test)]
mod tests {
    use super::*;
    use syn::parse_quote;

    fn arg(e: Expr, inputs: &[&str], binds: &[&str]) -> syn::Result<Arg> {
        let inputs = inputs.iter().map(|s| s.to_string()).collect();
        let binds = binds
            .iter()
            .map(|s| (s.to_string(), s.to_string()))
            .collect();
        expr_to_arg(&e, &binds, &inputs, None)
    }

    fn nodes(f: ItemFn) -> Vec<Node> {
        analyze_workflow(&f, "w").map(|r| r.0).expect("analyzes")
    }

    fn err(f: ItemFn) -> String {
        analyze_workflow(&f, "w")
            .err()
            .expect("rejected")
            .to_string()
    }

    #[test]
    fn task_names_dedup_and_skip_expr_reserved() {
        let mut used = Default::default();
        assert_eq!(uniq_task(&mut used, "make_list"), "make-list");
        assert_eq!(uniq_task(&mut used, "make_list"), "make-list-2");
        assert_eq!(uniq_task(&mut used, "if0"), "if0");
        assert_eq!(uniq_task(&mut used, "contains"), "contains-2");
        assert_eq!(uniq_task(&mut used, "contains"), "contains-3");
    }

    #[test]
    fn constant_seals_paths_keeps_ctor_func() {
        let e: Expr = parse_quote!(Opts {
            a: MAX,
            r: Some(cfg::X),
            t: (1, N),
            ..Opts::DEFAULT
        });
        let got = constant(&e, &|_| false).unwrap();
        let want: Expr = parse_quote!(Opts {
            a: const { MAX },
            r: Some(const { cfg::X }),
            t: (1, const { N }),
            ..const { Opts::DEFAULT }
        });
        assert_eq!(quote!(#got).to_string(), quote!(#want).to_string());
    }

    #[test]
    fn call_parts_skips_ctors() {
        assert!(call_parts(&parse_quote!(step(1))).is_some());
        assert!(call_parts(&parse_quote!(m::step(1))).is_some());
        assert!(call_parts(&parse_quote!(Some(1))).is_none());
        assert!(call_parts(&parse_quote!(Kind::A(1))).is_none());
    }

    #[test]
    fn literal_conversions_stay_macro_time() {
        for e in [
            parse_quote!("x".to_string()),
            parse_quote!("x".clone()),
            parse_quote!(3.to_owned()),
        ] {
            assert!(matches!(arg(e, &[], &[]), Ok(Arg::Lit(_))));
        }
        for e in [
            parse_quote!("x".into()),
            parse_quote!(-5),
            parse_quote!(5.to_string()),
            parse_quote!(MAX),
        ] {
            assert!(matches!(arg(e, &[], &[]), Ok(Arg::Const(_))));
        }
    }

    #[test]
    fn flow_in_constant_rejected() {
        let mixed = "can't be part of a constant value";
        let e = arg(parse_quote!(Opts { a: n }), &["n"], &[]).err().unwrap();
        assert!(e.to_string().contains(mixed), "{e}");
        let e = arg(parse_quote!((t, 1)), &[], &["t"]).err().unwrap();
        assert!(e.to_string().contains(mixed), "{e}");
        let e = err(parse_quote!(
            fn wf() {
                let l = mk();
                l.fan_out(|x| s(Opts { a: x }));
            }
        ));
        assert!(e.contains(mixed), "{e}");
    }

    #[test]
    fn constant_rejections() {
        let msg = |e: Expr| arg(e, &[], &[]).err().unwrap().to_string();
        assert_eq!(msg(parse_quote!(Opts { a: compute() })), CONST_CALL);
        assert_eq!(msg(parse_quote!(NAME.into())), CONST_INTO);
        for e in [
            parse_quote!(MAX * 2),
            parse_quote!(vec![1]),
            parse_quote!(A[0]),
            parse_quote!(X as u64),
            parse_quote!(Opts { a: &X }),
        ] {
            assert_eq!(msg(e), UNSUPPORTED_ARG);
        }
    }

    #[test]
    fn const_let_follows_rust_shadowing() {
        let n = nodes(parse_quote!(
            fn wf() {
                let n = 3;
                let n = step(n);
                sink(n);
            }
        ));
        assert!(matches!(&n[0].args[0], Arg::Lit(v) if v == "3"));
        assert!(matches!(&n[1].args[0], Arg::Ref(t) if t == "step"));
    }

    #[test]
    fn const_let_field_and_shorthand() {
        let n = nodes(parse_quote!(
            fn wf() {
                let a = 3;
                let cfg = Opts { a };
                step(cfg.a);
            }
        ));
        let Arg::Const(e) = &n[0].args[0] else {
            panic!("not a constant")
        };
        assert_eq!(quote!(#e).to_string(), "Opts { a : 3 } . a");
    }

    #[test]
    fn closure_param_shadows_const_let() {
        let n = nodes(parse_quote!(
            fn wf() {
                let x = 3;
                let l = mk();
                l.fan_out(|x| step(x));
            }
        ));
        assert!(matches!(&n[1].args[0], Arg::Item { path } if path.is_empty()));
    }

    #[test]
    fn inline_follows_scopes() {
        let mut s: Stmt = parse_quote!(if x > k {
            let k = mk();
            a(k).continue_on(failed);
        } else {
            l.fan_out(|k| b(k, failed));
        });
        let consts = [("k", parse_quote!(3)), ("failed", parse_quote!(4))];
        Inline(consts.map(|(n, v)| (n.to_string(), v)).into()).visit_stmt_mut(&mut s);
        let want: Stmt = parse_quote!(if x > 3 {
            let k = mk();
            a(k).continue_on(failed);
        } else {
            l.fan_out(|k| b(k, 4));
        });
        assert_eq!(quote!(#s).to_string(), quote!(#want).to_string());
    }

    #[test]
    fn let_alias_rejected() {
        let e = err(parse_quote!(
            fn wf(x: u32) {
                let y = x;
                step(y);
            }
        ));
        assert_eq!(e, LET_SHAPE);
    }
}
