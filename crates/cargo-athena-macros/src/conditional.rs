//! `if` → Argo `when` lowering + synthesized wrapper workflow generation.
//!
//! A whole `if`/`else if`/`else` chain in a `#[workflow]` becomes ONE
//! synthetic wrapper workflow (`<crate>-<fn>-if<k>`) whose DAG holds
//! one `when`-gated task per arm — gates mutually exclusive by
//! construction. Each arm body is itself a synthesized sub-`#[workflow]`
//! produced by re-entering [`crate::analyze::analyze_stmts`].
//!
//! - [`WhenExpr`] / [`cond_to_when`]: parse the Rust condition into a
//!   closed AST; [`render_when`] is the single producer of the Argo
//!   `when` string (so a malformed expression is unrepresentable).
//! - [`hoist_cond`]: pre-pass that pulls template-call operands in the
//!   condition into parent tasks (Rust evaluates conds unconditionally).
//! - [`synth_if`]: emits the wrapper + arm `SynthWf`s; the returned task
//!   name is what the parent dag/steps consumes (binding for value-`if`,
//!   side-effect task for stmt-`if`).
//! - [`emit_synth`]: lowers one `SynthWf` to a hidden
//!   `struct + impl Template` (Workflow-kind) — force-linked via the
//!   parent's `collect`.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, Path, Stmt};

use crate::analyze::{
    Arg, FALLIBLE_RETURN_UNSUPPORTED, FAN_RETURN_UNSUPPORTED, JsonSrc, Node, NodeOpts,
    RETURN_UNRESOLVED, analyze_stmts, call_parts, callee_paths, expr_to_arg, local_binding,
    path_leaf, push_call, retag_refs, uniq_task,
};
use crate::node_tokens::node_tokens;
use crate::utils::{str_slice, unwrap_expr, yaml_ambiguous};

#[derive(Clone, Copy)]
pub(crate) enum CmpOp {
    Eq,
    Ne,
    Lt,
    Le,
    Gt,
    Ge,
}

impl CmpOp {
    fn argo(self) -> &'static str {
        match self {
            CmpOp::Eq => "==",
            CmpOp::Ne => "!=",
            CmpOp::Lt => "<",
            CmpOp::Le => "<=",
            CmpOp::Gt => ">",
            CmpOp::Ge => ">=",
        }
    }
}

/// A condition operand. Like `Arg` but literals keep their kind (a
/// string compares as the JSON-quoted `"v"`, a number/bool bare — proven
/// on real Argo v4.0.5). Conditions are always resolved *inside* the
/// synthesized wrapper, where every parent binding/input arrives as a
/// wrapper input (hoisting + capture remapping happen first) — so the
/// only non-literal operands are input-rooted.
pub(crate) enum WhenOp {
    /// A wrapper input parameter (a captured parent binding/input).
    Input(String),
    /// `a.b.c` named-field access on an input (same lowering as
    /// `Arg::Json`).
    Json {
        input: String,
        path: Vec<String>,
    },
    Str(String),
    Int(String),
    Bool(bool),
}

/// Closed, parenthesized-on-render condition AST. The single `render`
/// (in the `if` synthesis) is the only producer of a `when` string, so a
/// malformed Argo expression is unrepresentable.
pub(crate) enum WhenExpr {
    Cmp {
        lhs: WhenOp,
        op: CmpOp,
        rhs: WhenOp,
    },
    And(Box<WhenExpr>, Box<WhenExpr>),
    Or(Box<WhenExpr>, Box<WhenExpr>),
    Not(Box<WhenExpr>),
    /// `if some_bool_binding` / `if !flag` leaf.
    Truthy(WhenOp),
}
pub(crate) const UNSUPPORTED_COND: &str = "unsupported `if` condition. Allowed: \
comparisons (== != < <= > >=) and `&&`/`||`/`!` over a #[workflow] input, \
a prior `let = template(...)` binding, an `a.field` of one, or a literal. \
Method calls, arithmetic, function calls and casts aren't lowered.";

/// Resolve a single condition operand, preserving literal kind. Reuses
/// the `expr_to_arg` field/binding/input rules so behaviour matches task
/// args exactly (incl. `.clone()`/`.to_owned()` passthrough).
pub(crate) fn cond_operand(
    e: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<WhenOp> {
    if let Expr::Lit(syn::ExprLit { lit, .. }) = unwrap_expr(e) {
        return Ok(match lit {
            syn::Lit::Str(s) => WhenOp::Str(s.value()),
            syn::Lit::Int(i) => WhenOp::Int(i.base10_digits().to_string()),
            syn::Lit::Float(f) => WhenOp::Int(f.base10_digits().to_string()),
            syn::Lit::Bool(b) => WhenOp::Bool(b.value),
            other => {
                return Err(syn::Error::new_spanned(other, UNSUPPORTED_COND));
            }
        });
    }
    // Non-literal operands share the exact arg rules (binding/input/
    // .field/.clone); map the resulting `Arg` into a `WhenOp`.
    match expr_to_arg(e, bindings, inputs) {
        Ok(Arg::Input(n)) => Ok(WhenOp::Input(n)),
        Ok(Arg::Json {
            src: JsonSrc::Input(n),
            path,
        }) => Ok(WhenOp::Json { input: n, path }),
        // `Arg::Lit` only comes back for a literal, handled above; `Item`
        // can't occur outside a fan_out closure; `Ref`/task-rooted `Json`
        // can't occur because conditions are resolved inside the wrapper
        // with empty bindings (parent bindings arrive as inputs).
        Ok(_) => Err(syn::Error::new_spanned(e, UNSUPPORTED_COND)),
        // Keep the resolver's own diagnosis (e.g. "`x` is not a
        // #[workflow] input …") — it names the actual problem; append
        // the condition-grammar context.
        Err(mut inner) => {
            inner.combine(syn::Error::new_spanned(e, UNSUPPORTED_COND));
            Err(inner)
        }
    }
}

/// Total lowering of a type-checked Rust condition into `WhenExpr`.
/// Anything outside the grammar is a spanned `compile_error!` — never a
/// mistranslation (consistent with the strict #[workflow] body contract).
pub(crate) fn cond_to_when(
    e: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<WhenExpr> {
    match unwrap_expr(e) {
        Expr::Binary(b) => {
            use syn::BinOp::*;
            let op = match b.op {
                Eq(_) => Some(CmpOp::Eq),
                Ne(_) => Some(CmpOp::Ne),
                Lt(_) => Some(CmpOp::Lt),
                Le(_) => Some(CmpOp::Le),
                Gt(_) => Some(CmpOp::Gt),
                Ge(_) => Some(CmpOp::Ge),
                _ => None,
            };
            if let Some(op) = op {
                return Ok(WhenExpr::Cmp {
                    lhs: cond_operand(&b.left, bindings, inputs)?,
                    op,
                    rhs: cond_operand(&b.right, bindings, inputs)?,
                });
            }
            match b.op {
                And(_) => Ok(WhenExpr::And(
                    Box::new(cond_to_when(&b.left, bindings, inputs)?),
                    Box::new(cond_to_when(&b.right, bindings, inputs)?),
                )),
                Or(_) => Ok(WhenExpr::Or(
                    Box::new(cond_to_when(&b.left, bindings, inputs)?),
                    Box::new(cond_to_when(&b.right, bindings, inputs)?),
                )),
                _ => Err(syn::Error::new_spanned(b, UNSUPPORTED_COND)),
            }
        }
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => Ok(WhenExpr::Not(Box::new(
            cond_to_when(&u.expr, bindings, inputs)?,
        ))),
        // A bare operand condition: `if flag` / `if a.enabled`.
        other => Ok(WhenExpr::Truthy(cond_operand(other, bindings, inputs)?)),
    }
}

/// A single condition operand: if it's a template call (`if foo() > 3`),
/// hoist it to a parent task (Rust evaluates the condition regardless of
/// branch, so it runs unconditionally) and substitute a reference to it;
/// identical calls within one `if` are hoisted once. Otherwise unchanged.
#[allow(clippy::too_many_arguments)]
pub(crate) fn hoist_operand(
    e: &Expr,
    used: &mut std::collections::HashSet<String>,
    nodes: &mut Vec<Node>,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
    seen: &mut std::collections::HashMap<String, syn::Ident>,
    cond_binds: &mut std::collections::HashMap<String, String>,
) -> syn::Result<Expr> {
    if let Some((c2, r2)) = call_parts(e) {
        let key = quote!(#e).to_string();
        let id = if let Some(id) = seen.get(&key) {
            id.clone()
        } else {
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
            let id = format_ident!("__athena_cond_{}", seen.len());
            cond_binds.insert(id.to_string(), t);
            seen.insert(key, id.clone());
            id
        };
        Ok(syn::parse_quote!(#id))
    } else {
        Ok(e.clone())
    }
}

/// Rewrite a condition, hoisting every template-call operand (recursive
/// over the `== != < <= > >=`, `&&`, `||`, `!` grammar so
/// `a && foo() == bar()` hoists both). Non-grammar shapes pass through
/// unchanged — `cond_to_when` then produces the proper error.
#[allow(clippy::too_many_arguments)]
pub(crate) fn hoist_cond(
    e: &Expr,
    used: &mut std::collections::HashSet<String>,
    nodes: &mut Vec<Node>,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
    seen: &mut std::collections::HashMap<String, syn::Ident>,
    cond_binds: &mut std::collections::HashMap<String, String>,
) -> syn::Result<Expr> {
    match unwrap_expr(e) {
        Expr::Binary(b) => {
            use syn::BinOp::*;
            let mut nb = b.clone();
            let (l, r) = match b.op {
                Eq(_) | Ne(_) | Lt(_) | Le(_) | Gt(_) | Ge(_) => (
                    hoist_operand(&b.left, used, nodes, bindings, inputs, seen, cond_binds)?,
                    hoist_operand(&b.right, used, nodes, bindings, inputs, seen, cond_binds)?,
                ),
                And(_) | Or(_) => (
                    hoist_cond(&b.left, used, nodes, bindings, inputs, seen, cond_binds)?,
                    hoist_cond(&b.right, used, nodes, bindings, inputs, seen, cond_binds)?,
                ),
                _ => return Ok(e.clone()),
            };
            nb.left = Box::new(l);
            nb.right = Box::new(r);
            Ok(Expr::Binary(nb))
        }
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => {
            let mut nu = u.clone();
            nu.expr = Box::new(hoist_cond(
                &u.expr, used, nodes, bindings, inputs, seen, cond_binds,
            )?);
            Ok(Expr::Unary(nu))
        }
        _ => hoist_operand(e, used, nodes, bindings, inputs, seen, cond_binds),
    }
}

/// What a (synthetic or real) workflow's `outputs.parameters.return`
/// resolves to.
pub(crate) enum SynthOut {
    /// No return value (statement-position `if`, void workflow).
    None,
    /// Bubble a terminal task's `return` (`valueFrom.parameter`).
    Terminal(String),
    /// `if`/`else` value selection: pick the arm task that Succeeded
    /// (`valueFrom.expression` status-ternary — proven on Argo v4.0.5).
    Select(Vec<String>),
}

/// A macro-synthesized `#[workflow]`-equivalent (an `if` wrapper or one
/// of its arm bodies). Emitted as an extra `struct + impl Template` in
/// the parent's expansion; force-linked via the parent's `collect`.
pub(crate) struct SynthWf {
    ident: syn::Ident,
    argo_name: String,
    inputs: Vec<String>,
    nodes: Vec<Node>,
    callees: Vec<Path>,
    output: SynthOut,
}

/// Single-segment identifiers *referenced* anywhere in `stmts` (call
/// args, conditions, field bases, fan_out receivers, …), minus the names
/// those statements bind locally. Used to compute an `if` arm's captured
/// free variables. Order = first appearance (stable Argo params).
pub(crate) fn referenced_idents(stmts: &[Stmt]) -> Vec<String> {
    use syn::visit::Visit;
    struct Scan {
        seen: Vec<String>,
        set: std::collections::HashSet<String>,
    }
    impl<'a> Visit<'a> for Scan {
        fn visit_expr_path(&mut self, p: &'a syn::ExprPath) {
            if p.qself.is_none()
                && let Some(id) = p.path.get_ident()
            {
                let s = id.to_string();
                if self.set.insert(s.clone()) {
                    self.seen.push(s);
                }
            }
            syn::visit::visit_expr_path(self, p);
        }
    }
    let mut s = Scan {
        seen: Vec::new(),
        set: Default::default(),
    };
    let blk = syn::Block {
        brace_token: Default::default(),
        stmts: stmts.to_vec(),
    };
    s.visit_block(&blk);
    s.seen
}

/// Names bound by `let` (or `_`) directly in `stmts` — excluded from a
/// block's free set.
pub(crate) fn locally_bound(stmts: &[Stmt]) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for st in stmts {
        if let Stmt::Local(l) = st
            && let Ok(Some(n)) = local_binding(&l.pat)
        {
            out.insert(n);
        }
    }
    out
}

/// Flatten an `if` / `else if` / `else` chain into ordered arms. Each is
/// `(Some(cond), body)`; the trailing `(None, body)` is the `else`.
pub(crate) fn if_arms(mut e: &syn::ExprIf) -> Vec<(Option<Expr>, syn::Block)> {
    let mut arms: Vec<(Option<Expr>, syn::Block)> = Vec::new();
    loop {
        arms.push(((*e.cond).clone().into(), e.then_branch.clone()));
        match e.else_branch.as_ref().map(|(_, b)| b.as_ref()) {
            Some(Expr::If(nested)) => e = nested,
            Some(Expr::Block(b)) => {
                arms.push((None, b.block.clone()));
                break;
            }
            _ => break, // no `else`
        }
    }
    arms
}

pub(crate) fn when_op_str(o: &WhenOp) -> String {
    match o {
        WhenOp::Input(n) => format!("{{{{inputs.parameters.{n}}}}}"),
        WhenOp::Json { input, path } => {
            let acc: String = path.iter().map(|f| format!("['{f}']")).collect();
            format!("{{{{=toJSON(fromJSON(inputs.parameters['{input}']){acc})}}}}")
        }
        WhenOp::Str(s) => {
            format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
        }
        WhenOp::Int(s) => s.clone(),
        WhenOp::Bool(b) => if *b { "true" } else { "false" }.to_string(),
    }
}

/// The only producer of a `when` string — parenthesized so we never lean
/// on expr-lang's precedence table (valid-by-construction).
pub(crate) fn render_when(w: &WhenExpr) -> String {
    match w {
        WhenExpr::Cmp { lhs, op, rhs } => {
            format!("({} {} {})", when_op_str(lhs), op.argo(), when_op_str(rhs))
        }
        WhenExpr::And(a, b) => {
            format!("({} && {})", render_when(a), render_when(b))
        }
        WhenExpr::Or(a, b) => {
            format!("({} || {})", render_when(a), render_when(b))
        }
        WhenExpr::Not(x) => format!("!({})", render_when(x)),
        WhenExpr::Truthy(o) => format!("({})", when_op_str(o)),
    }
}

/// `valueFrom.expression` selecting the arm task that Succeeded
/// (right-folded ternary; the last arm is the unconditional fallback —
/// the `else`, or the only arm). Proven on real Argo v4.0.5.
pub(crate) fn select_expr(arms: &[String]) -> String {
    let mut it = arms.iter().rev();
    let last = it.next().expect("if has at least one arm");
    let mut acc = format!("tasks['{last}'].outputs.parameters.return");
    for a in it {
        acc = format!(
            "tasks['{a}'].status == 'Succeeded' ? \
             tasks['{a}'].outputs.parameters.return : {acc}"
        );
    }
    acc
}

/// Record a `callee(raw...)` call (+ peeled builder opts) as a node;
/// returns its task name. (Free fn so the analyzer can recurse into `if`
/// arm bodies.)
#[allow(clippy::too_many_arguments)]
pub(crate) fn synth_if(
    ei: &syn::ExprIf,
    bind: Option<&str>,
    value: bool,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
    used: &mut std::collections::HashSet<String>,
    nodes: &mut Vec<Node>,
    ctx: &mut SynthCtx,
) -> syn::Result<String> {
    let arms = if_arms(ei);
    let has_else = arms.last().map(|(c, _)| c.is_none()).unwrap_or(false);
    if value && !has_else {
        return Err(syn::Error::new_spanned(
            ei,
            "an `if` used as a value needs an `else` (both branches must \
             produce the value) — Rust requires this too.",
        ));
    }

    // Hoist template-call operands in conditions to parent tasks
    // (`if foo() > 3` → a parent `foo` node + `__athena_cond_N` ref);
    // identical calls share one task. Conditions are then plain
    // binding/input/field/literal expressions again.
    let mut seen_calls: std::collections::HashMap<String, syn::Ident> = Default::default();
    let mut cond_binds: std::collections::HashMap<String, String> = Default::default();
    let arms: Vec<(Option<Expr>, syn::Block)> = arms
        .into_iter()
        .map(|(c, b)| -> syn::Result<_> {
            let c = match c {
                Some(c) => Some(hoist_cond(
                    &c,
                    used,
                    nodes,
                    bindings,
                    inputs,
                    &mut seen_calls,
                    &mut cond_binds,
                )?),
                None => None,
            };
            Ok((c, b))
        })
        .collect::<syn::Result<_>>()?;
    // Parent scope augmented with the hoisted condition tasks.
    let mut eff_bindings = bindings.clone();
    for (kk, vv) in &cond_binds {
        eff_bindings.insert(kk.clone(), vv.clone());
    }

    // Captured free vars = (every arm body ∪ every condition) ∩ parent
    // scope. Whole bindings/inputs only (a field/`.f` captures its base).
    let mut refset: Vec<String> = Vec::new();
    let pushref = |v: Vec<String>, refset: &mut Vec<String>| {
        for n in v {
            if !refset.contains(&n) {
                refset.push(n);
            }
        }
    };
    for (cond, body) in &arms {
        let local = locally_bound(&body.stmts);
        let mut rs = referenced_idents(&body.stmts);
        rs.retain(|n| !local.contains(n));
        pushref(rs, &mut refset);
        if let Some(c) = cond {
            pushref(
                referenced_idents(std::slice::from_ref(&Stmt::Expr(c.clone(), None))),
                &mut refset,
            );
        }
    }
    let captures: Vec<String> = refset
        .into_iter()
        .filter(|n| eff_bindings.contains_key(n) || inputs.contains(n))
        .collect();
    for c in &captures {
        if let Some(why) = yaml_ambiguous(c) {
            return Err(syn::Error::new_spanned(
                ei,
                format!(
                    "the `if` captures `{c}`, which becomes an Argo \
                     parameter name a YAML 1.1 parser reads as {why}. \
                     Rename that binding/input."
                ),
            ));
        }
    }
    // Capture scope: inside the wrapper/arms every capture is an input.
    let cap_inputs: std::collections::HashSet<String> = captures.iter().cloned().collect();
    let empty_bindings = std::collections::HashMap::new();

    let k = ctx.if_ctr;
    ctx.if_ctr += 1;
    let wrap_ident = format_ident!("__athena_{}_if{}", ctx.parent_rust, k);
    let wrap_argo = format!("{}-if{}", ctx.parent_argo, k);

    // One sub-workflow + one when-gated wrapper task per arm.
    let mut wrap_nodes: Vec<Node> = Vec::new();
    let mut arm_tasks: Vec<String> = Vec::new();
    for (j, (_, body)) in arms.iter().enumerate() {
        let arm_ident = format_ident!("__athena_{}_if{}_arm{}", ctx.parent_rust, k, j);
        let arm_argo = format!("{wrap_argo}-arm{j}");
        let (mut anodes, aout) = analyze_stmts(
            &body.stmts,
            &cap_inputs,
            value,
            &arm_argo,
            &format!("{}_if{}_arm{}", ctx.parent_rust, k, j),
            ctx,
        )?;
        // A value-`if` arm must produce the value — report it with the
        // arm's own span (analyze_stmts leaves this to its callers so
        // the top-level workflow can point at the return type instead).
        if value && aout.is_none() {
            return Err(syn::Error::new_spanned(body, RETURN_UNRESOLVED));
        }
        // Each arm body is its own scope: re-tag refs and reject fan /
        // fallible bindings as the arm's value (same as top level).
        let (arm_fan_tasks, arm_fallible) = retag_refs(&mut anodes);
        if let Some(t) = &aout
            && value
        {
            if arm_fan_tasks.contains(t) {
                return Err(syn::Error::new_spanned(body, FAN_RETURN_UNSUPPORTED));
            }
            if arm_fallible.contains(t) {
                return Err(syn::Error::new_spanned(body, FALLIBLE_RETURN_UNSUPPORTED));
            }
        }
        let acallees = callee_paths(&anodes, &[]);
        ctx.synth.push(SynthWf {
            ident: arm_ident.clone(),
            argo_name: arm_argo,
            inputs: captures.clone(),
            nodes: anodes,
            callees: acallees,
            output: match aout {
                Some(t) if value => SynthOut::Terminal(t),
                _ => SynthOut::None,
            },
        });

        // Gate j = !c0 && … && !c{j-1} && cj  (else arm: just the !c's).
        let mut gate: Option<WhenExpr> = None;
        let conj = |g: &mut Option<WhenExpr>, w: WhenExpr| {
            *g = Some(match g.take() {
                None => w,
                Some(p) => WhenExpr::And(Box::new(p), Box::new(w)),
            });
        };
        for (cond, _) in &arms[..j] {
            if let Some(c) = cond {
                conj(
                    &mut gate,
                    WhenExpr::Not(Box::new(cond_to_when(c, &empty_bindings, &cap_inputs)?)),
                );
            }
        }
        if let Some(c) = &arms[j].0 {
            conj(&mut gate, cond_to_when(c, &empty_bindings, &cap_inputs)?);
        }
        let task = format!("arm{j}");
        arm_tasks.push(task.clone());
        wrap_nodes.push(Node {
            task,
            callee: syn::parse_quote!(#arm_ident),
            args: captures.iter().cloned().map(Arg::Input).collect(),
            continue_on: None,
            hooks: Vec::new(),
            fan: None,
            when: gate.as_ref().map(render_when),
        });
    }

    let wrap_callees = callee_paths(&wrap_nodes, &[]);
    ctx.synth.push(SynthWf {
        ident: wrap_ident.clone(),
        argo_name: wrap_argo,
        inputs: captures.clone(),
        nodes: wrap_nodes,
        callees: wrap_callees,
        output: if value {
            SynthOut::Select(arm_tasks)
        } else {
            SynthOut::None
        },
    });

    // Parent calls the wrapper exactly like a returning sub-workflow.
    let base = bind.map(str::to_string).unwrap_or_else(|| "if".into());
    let parent_args = captures
        .iter()
        .map(|n| {
            if let Some(t) = eff_bindings.get(n) {
                Arg::Ref(t.clone())
            } else {
                Arg::Input(n.clone())
            }
        })
        .collect();
    let task = uniq_task(used, &base);
    nodes.push(Node {
        task: task.clone(),
        callee: syn::parse_quote!(#wrap_ident),
        args: parent_args,
        continue_on: None,
        hooks: Vec::new(),
        fan: None,
        when: None,
    });
    Ok(task)
}

/// Per-top-workflow synthesis context: accumulates `if` wrappers/arms
/// (emitted flat in the parent expansion) and a global counter.
pub(crate) struct SynthCtx {
    pub(crate) synth: Vec<SynthWf>,
    pub(crate) if_ctr: u32,
    pub(crate) parent_rust: String,
    pub(crate) parent_argo: String,
}
pub(crate) fn emit_synth(s: &SynthWf) -> TokenStream2 {
    let ident = &s.ident;
    let argo = &s.argo_name;
    let inputs_slice = str_slice(&s.inputs);
    let node_blocks: Vec<_> = s.nodes.iter().map(|n| node_tokens(n, false)).collect();
    let names = &s.inputs;
    let inputs_tokens = if names.is_empty() {
        quote! { ::core::option::Option::None }
    } else {
        quote! {
            ::core::option::Option::Some(::cargo_athena::api::Inputs {
                parameters: ::std::vec![
                    #( ::cargo_athena::api::Parameter {
                        name: #names.to_string(),
                        ..::core::default::Default::default()
                    } ),*
                ],
                ..::core::default::Default::default()
            })
        }
    };
    let outputs_tokens = match &s.output {
        SynthOut::None => quote! {},
        SynthOut::Terminal(t) => {
            let refstr = format!("{{{{tasks.{t}.outputs.parameters.return}}}}");
            quote! {
                outputs: ::core::option::Option::Some(
                    ::cargo_athena::api::Outputs {
                        parameters: ::std::vec![
                            ::cargo_athena::api::Parameter {
                                name: "return".to_string(),
                                value_from: ::core::option::Option::Some(
                                    ::cargo_athena::api::ValueFrom {
                                        parameter: #refstr.to_string(),
                                        ..::core::default::Default::default()
                                    }
                                ),
                                ..::core::default::Default::default()
                            }
                        ],
                        ..::core::default::Default::default()
                    }
                ),
            }
        }
        SynthOut::Select(arms) => {
            let exprstr = select_expr(arms);
            quote! {
                outputs: ::core::option::Option::Some(
                    ::cargo_athena::api::Outputs {
                        parameters: ::std::vec![
                            ::cargo_athena::api::Parameter {
                                name: "return".to_string(),
                                value_from: ::core::option::Option::Some(
                                    ::cargo_athena::api::ValueFrom {
                                        expression: #exprstr.to_string(),
                                        ..::core::default::Default::default()
                                    }
                                ),
                                ..::core::default::Default::default()
                            }
                        ],
                        ..::core::default::Default::default()
                    }
                ),
            }
        }
    };
    let callees = &s.callees;
    quote! {
        #[allow(non_camel_case_types)]
        struct #ident;
        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const SYNTHETIC: bool = true;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Workflow;

            fn build(_ctx: &::cargo_athena::BuildCtx)
                -> ::cargo_athena::api::Template
            {
                let mut __tasks: ::std::vec::Vec<
                    ::cargo_athena::api::DagTask,
                > = ::std::vec::Vec::new();
                #( #node_blocks )*
                ::cargo_athena::api::Template {
                    name: <Self as ::cargo_athena::Template>::ARGO_NAME
                        .to_string(),
                    inputs: #inputs_tokens,
                    dag: ::core::option::Option::Some(
                        ::cargo_athena::api::DagTemplate { tasks: __tasks }),
                    #outputs_tokens
                    ..::core::default::Default::default()
                }
            }

            fn collect(__out: &mut ::cargo_athena::Collector) {
                if !__out.enter::<Self>() {
                    return;
                }
                __out.add::<Self>();
                #(
                    <#callees as ::cargo_athena::Template>::collect(__out);
                )*
            }
        }
    }
}
