//! Proc macros that compile annotated Rust fns into Argo templates.
//!
//! * `#[container]` — leaf. The fn body stays real code (run in-pod); we
//!   add a run-mode dispatcher and a template builder. `host!`/callees are
//!   collected **statically** (AST union over every branch) so resource
//!   declarations are never missed regardless of control flow.
//! * `#[workflow]` — composition. The body is *analyzed*, not compiled:
//!   straight-line `let x = callee(args);` becomes a DAG; data deps become
//!   Argo parameter wiring. (Hybrid seam: this is the static analyzer that
//!   later lowers into the promise-graph for richer control flow.)
//! * `#[fragment]` — a plain helper fn that carries `host!` decls; it
//!   propagates resources up the static call graph (cross-item case).

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, ItemFn, Path, Stmt, Type, visit::Visit, visit_mut::VisitMut};

// ---------------------------------------------------------------------------
// shared helpers
// ---------------------------------------------------------------------------

fn kebab(s: &str) -> String {
    s.replace('_', "-").to_ascii_lowercase()
}

/// `(ident, type)` for each non-receiver fn argument.
fn fn_args(func: &ItemFn) -> Vec<(syn::Ident, Box<Type>)> {
    func.sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            syn::FnArg::Typed(pt) => match &*pt.pat {
                syn::Pat::Ident(p) => Some((p.ident.clone(), pt.ty.clone())),
                _ => None,
            },
            syn::FnArg::Receiver(_) => None,
        })
        .collect()
}

/// The declaration macros the attribute macros statically collect + gate.
/// `(public ident, private ident, which `BodyScan` bucket)`.
const DECL_MACROS: &[(&str, &str, DeclKind)] = &[
    ("host", "__cargo_athena_host", DeclKind::Host),
    (
        "load_artifact",
        "__cargo_athena_load_artifact",
        DeclKind::InArtifact,
    ),
    (
        "load_artifact_str",
        "__cargo_athena_load_artifact_str",
        DeclKind::InArtifact,
    ),
    (
        "save_artifact",
        "__cargo_athena_save_artifact",
        DeclKind::OutArtifact,
    ),
    (
        "save_artifact_str",
        "__cargo_athena_save_artifact_str",
        DeclKind::OutArtifact,
    ),
]; // NB: keep in sync with the macro pairs in cargo-athena-core.

#[derive(Clone, Copy, PartialEq)]
enum DeclKind {
    Host,
    InArtifact,
    OutArtifact,
}

fn decl_kind(mac: &syn::Macro) -> Option<(DeclKind, &'static str)> {
    let last = mac.path.segments.last()?;
    DECL_MACROS
        .iter()
        .find(|(public, ..)| last.ident == public)
        .map(|(_, private, kind)| (*kind, *private))
}

/// First string-literal argument of a decl macro (`host!("p")`,
/// `save_artifact!("n", expr)` → `"n"`). Literal-only by contract.
fn first_str_lit(mac: &syn::Macro) -> Option<String> {
    let args = mac
        .parse_body_with(
            syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated,
        )
        .ok()?;
    match args.first()? {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// Static collector: every decl-macro literal (across all branches) + every
/// called ident, used for the cross-item `#[fragment]` closure.
#[derive(Default)]
struct BodyScan {
    host_paths: Vec<String>,
    in_artifacts: Vec<String>,
    out_artifacts: Vec<String>,
    callees: Vec<String>,
}

impl<'ast> Visit<'ast> for BodyScan {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some((kind, _)) = decl_kind(mac)
            && let Some(name) = first_str_lit(mac)
        {
            match kind {
                DeclKind::Host => self.host_paths.push(name),
                DeclKind::InArtifact => self.in_artifacts.push(name),
                DeclKind::OutArtifact => self.out_artifacts.push(name),
            }
        }
        syn::visit::visit_macro(self, mac);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        if let Expr::Path(p) = &*call.func
            && p.path.segments.len() == 1
        {
            self.callees.push(p.path.segments[0].ident.to_string());
        }
        syn::visit::visit_expr_call(self, call);
    }
}

fn scan_body(func: &ItemFn) -> BodyScan {
    let mut s = BodyScan::default();
    s.visit_block(&func.block);
    for v in [
        &mut s.host_paths,
        &mut s.in_artifacts,
        &mut s.out_artifacts,
        &mut s.callees,
    ] {
        v.sort();
        v.dedup();
    }
    s
}

fn str_slice(items: &[String]) -> TokenStream2 {
    let lits = items.iter().map(|s| s.as_str());
    quote! { &[ #( #lits ),* ] }
}

/// Defining crate's name (set by Cargo while this crate compiles), used to
/// namespace Argo template names so they're globally unique across crates.
fn crate_ns() -> String {
    std::env::var("CARGO_CRATE_NAME")
        .or_else(|_| std::env::var("CARGO_PKG_NAME"))
        .unwrap_or_else(|_| "crate".to_string())
}

/// Final Argo resource name: an explicit `name = "..."` override, else
/// `<crate>-<fn>` (kebab, DNS-1123-ish, globally unique).
fn make_argo_name(name_override: &Option<String>, rust_name: &str) -> String {
    match name_override {
        Some(n) => n.clone(),
        None => format!("{}-{}", kebab(&crate_ns()), kebab(rust_name)),
    }
}

// Attribute args, parsed with `deluxe` (all fields optional/defaulted).

/// `#[container(image = "...", name = "...", service_account = "...",
///   node_selector = { "k" = "v", ... })]`
#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
struct ContainerArgs {
    image: Option<String>,
    name: Option<String>,
    service_account: Option<String>,
    node_selector: std::collections::BTreeMap<String, String>,
    /// `on_exit = path::to::template` — wired onto the runnable
    /// Workflow's `spec.onExit` when this is the emit root.
    on_exit: Option<syn::Path>,
}

/// `#[workflow(name = "...", steps, on_exit = teardown)]` — bare `steps`
/// opts into Argo `steps:` (sequential) instead of the default `dag:`.
#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
struct WorkflowArgs {
    name: Option<String>,
    steps: deluxe::Flag,
    on_exit: Option<syn::Path>,
}

/// Parse attribute args into `T`, or return a `compile_error!`.
fn parse_attr<T: deluxe::ParseMetaItem + Default>(attr: TokenStream) -> Result<T, TokenStream> {
    if attr.is_empty() {
        return Ok(T::default());
    }
    deluxe::parse2::<T>(attr.into()).map_err(|e| e.into_compile_error().into())
}

/// Rewrites every decl macro (`host!`, `load_artifact*!`,
/// `save_artifact*!`) the attribute macro can see into its private real
/// form. Enforcement half of the gate: the *public* forms are hard
/// `compile_error!`s, so any invocation we don't rewrite here — a plain
/// fn, a `#[workflow]`, or nested inside another macro's tokens — fails to
/// compile instead of silently doing nothing.
struct DeclRewrite;

impl VisitMut for DeclRewrite {
    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        if let Some((_, private)) = decl_kind(mac) {
            let p: syn::Path =
                syn::parse_str(&format!("::cargo_athena::{private}")).unwrap();
            mac.path = p;
        }
        syn::visit_mut::visit_macro_mut(self, mac);
    }
}

/// Clone `func`, swap its visible decl macros for the private forms; the
/// original is left intact for the (pre-rewrite) static scan.
fn with_host_rewritten(func: &ItemFn) -> ItemFn {
    let mut out = func.clone();
    DeclRewrite.visit_item_fn_mut(&mut out);
    out
}

// ---------------------------------------------------------------------------
// #[container]
// ---------------------------------------------------------------------------

#[proc_macro_attribute]
pub fn container(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let cfg: ContainerArgs = match parse_attr(attr) {
        Ok(c) => c,
        Err(e) => return e,
    };

    let ident = func.sig.ident.clone();
    let rust_name = ident.to_string();
    let argo_name = make_argo_name(&cfg.name, &rust_name);
    let scan = scan_body(&func);

    // The real body becomes a hidden fn; the public identity is a type.
    let mut impl_fn = with_host_rewritten(&func);
    let impl_ident = format_ident!("__cargo_athena_impl_{}", ident);
    impl_fn.sig.ident = impl_ident.clone();
    impl_fn.vis = syn::Visibility::Inherited;

    let args = fn_args(&func);
    let arg_idents: Vec<_> = args.iter().map(|(i, _)| i.clone()).collect();
    let arg_types: Vec<_> = args.iter().map(|(_, t)| t.clone()).collect();
    let arg_names: Vec<String> = arg_idents.iter().map(|i| i.to_string()).collect();

    // Argo delivers params via container env so the binary can read them.
    let param_env_names: Vec<String> =
        arg_names.iter().map(|n| format!("ATHENA_PARAM_{n}")).collect();
    let param_env_vals: Vec<String> = arg_names
        .iter()
        .map(|n| format!("{{{{inputs.parameters.{n}}}}}"))
        .collect();
    let inputs_slice = str_slice(&arg_names);
    let host_slice = str_slice(&scan.host_paths);
    let in_art_slice = str_slice(&scan.in_artifacts);
    let out_art_slice = str_slice(&scan.out_artifacts);
    let callee_slice = str_slice(&scan.callees);
    let image_opt = match &cfg.image {
        Some(img) => quote! { ::core::option::Option::Some(#img) },
        None => quote! { ::core::option::Option::None },
    };
    let sa_opt = match &cfg.service_account {
        Some(sa) => quote! { ::core::option::Option::Some(#sa) },
        None => quote! { ::core::option::Option::None },
    };
    let ns_keys: Vec<&String> = cfg.node_selector.keys().collect();
    let ns_vals: Vec<&String> = cfg.node_selector.values().collect();
    let vis = &func.vis;

    // `#[container(on_exit = t)]`: Template::ON_EXIT (root-only effect) +
    // force-link/emit the handler template.
    let (on_exit_const, on_exit_collect) = match &cfg.on_exit {
        Some(p) => (
            quote! {
                const ON_EXIT: ::core::option::Option<&'static str> =
                    ::core::option::Option::Some(
                        <#p as ::cargo_athena::Template>::ARGO_NAME,
                    );
            },
            quote! { <#p as ::cargo_athena::Template>::collect(__out); },
        ),
        None => (quote! {}, quote! {}),
    };

    let expanded = quote! {
        // Hidden real implementation — executed in-pod (Run mode).
        #impl_fn

        // The importable identity: a type, not a fn.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo_name;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Container;
            #on_exit_const

            fn run(__in: ::cargo_athena::serde_json::Value)
                -> ::cargo_athena::serde_json::Value
            {
                #(
                    let #arg_idents: #arg_types =
                        ::cargo_athena::serde_json::from_value(
                            __in.get(#arg_names)
                                .cloned()
                                .unwrap_or(::cargo_athena::serde_json::Value::Null),
                        ).expect(concat!(
                            "deserialize container input `", #arg_names, "`"));
                )*
                let __out = #impl_ident( #( #arg_idents ),* );
                ::cargo_athena::serde_json::to_value(__out)
                    .expect("serialize container output")
            }

            fn build(__ctx: &::cargo_athena::BuildCtx)
                -> ::cargo_athena::api::Template
            {
                let __paths = __ctx.resolved_host_paths(
                    #host_slice, #callee_slice);
                // emptyDir scratch at /athena (+ host! volumes) so every
                // athena path is writable on any image.
                let (__vols, __mounts) =
                    ::cargo_athena::container_volumes(&__paths);
                // Arbitrary user image + the arch-resolving bootstrap that
                // pulls & exec's the athena binary delivered as an artifact.
                let __d = ::cargo_athena::container_delivery(
                    __ctx,
                    <Self as ::cargo_athena::Template>::ARGO_NAME,
                    #image_opt,
                );
                // Native Argo artifact ports (no S3): own load/save decls
                // ∪ the #[fragment] closure.
                let __in_names = __ctx.resolved_in_artifacts(
                    #in_art_slice, #callee_slice);
                let __out_names = __ctx.resolved_out_artifacts(
                    #out_art_slice, #callee_slice);
                let mut __in_artifacts = ::std::vec![ __d.artifact ];
                __in_artifacts.extend(
                    ::cargo_athena::artifact_inputs(__ctx, &__in_names));
                ::cargo_athena::api::Template {
                    name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                    inputs: ::core::option::Option::Some(::cargo_athena::api::Inputs {
                        parameters: ::std::vec![
                            #( ::cargo_athena::api::Parameter {
                                name: #arg_names.to_string(),
                                ..::core::default::Default::default()
                            } ),*
                        ],
                        artifacts: __in_artifacts,
                    }),
                    outputs: ::core::option::Option::Some(::cargo_athena::api::Outputs {
                        parameters: ::std::vec![ ::cargo_athena::api::Parameter {
                            // Named `return` (NOT `result`): `outputs.result`
                            // is Argo's script-stdout alias — a distinct
                            // thing. This is the serialized fn return value,
                            // captured from the /athena/result file.
                            name: "return".to_string(),
                            value_from: ::core::option::Option::Some(
                                ::cargo_athena::api::ValueFrom {
                                    path: "/athena/result".to_string(),
                                    ..::core::default::Default::default()
                                }
                            ),
                            ..::core::default::Default::default()
                        } ],
                        artifacts: ::cargo_athena::artifact_outputs(__ctx, &__out_names),
                    }),
                    container: ::core::option::Option::Some(::cargo_athena::api::Container {
                        image: __d.image,
                        command: __d.command,
                        args: __d.args,
                        env: ::std::vec![
                            #( ::cargo_athena::api::EnvVar {
                                name: #param_env_names.to_string(),
                                value: #param_env_vals.to_string(),
                            } ),*
                        ],
                        volume_mounts: __mounts,
                        ..::core::default::Default::default()
                    }),
                    volumes: __vols,
                    service_account_name:
                        ::cargo_athena::service_account(__ctx, #sa_opt),
                    node_selector: {
                        let mut __ns = ::std::collections::BTreeMap::new();
                        #( __ns.insert(
                            #ns_keys.to_string(), #ns_vals.to_string()); )*
                        __ns
                    },
                    ..::core::default::Default::default()
                }
            }

            fn collect(__out: &mut ::cargo_athena::Collector) {
                if !__out.enter(<Self as ::cargo_athena::Template>::ARGO_NAME) {
                    return;
                }
                __out.add_builder(<Self as ::cargo_athena::Template>::build);
                __out.add_runner(
                    <Self as ::cargo_athena::Template>::ARGO_NAME,
                    <Self as ::cargo_athena::Template>::run,
                );
                #on_exit_collect
            }
        }
    };
    expanded.into()
}

// ---------------------------------------------------------------------------
// #[workflow]
// ---------------------------------------------------------------------------

enum Arg {
    /// Literal/const value passed straight through as an Argo parameter.
    Lit(String),
    /// Reference to a previous task's output (creates a DAG dependency).
    Ref(String),
    /// Reference to one of the workflow's own input parameters.
    Input(String),
}

/// A hook peeled off a call, args still as raw `Expr` (resolved against
/// bindings/inputs later in `push_call`). `expression: None` == the
/// special `exit` hook; `Some(e)` runs when the Argo expression holds.
struct HookSpec {
    expression: Option<String>,
    /// Hook template path — force-linked + emitted as a `templateRef`
    /// exactly like a callee.
    template: Path,
    /// `.on_exit(t(args))` — args to the hook template (empty for a bare
    /// path or for `.hooks(...)`, whose arg-grammar is still deferred).
    raw_args: Vec<Expr>,
}

/// Per-task builders peeled off a call: `.continue_on(...)`, `.hooks(...)`,
/// `.on_exit(...)`.
#[derive(Default)]
struct NodeOpts {
    /// `(error, failed)` for Argo `continueOn`.
    continue_on: Option<(bool, bool)>,
    hooks: Vec<HookSpec>,
}

/// A hook with its args resolved to `Arg`s (post `push_call`).
struct Hook {
    expression: Option<String>,
    template: Path,
    args: Vec<Arg>,
}

struct Node {
    task: String,
    /// Callee path exactly as written (`ingest`, `foo::ingest`, …) — used
    /// as a *type* in `<callee as Template>` so the compiler resolves its
    /// Argo name/inputs across modules and crates and force-links it.
    callee: Path,
    args: Vec<Arg>,
    continue_on: Option<(bool, bool)>,
    hooks: Vec<Hook>,
}

fn unwrap_expr(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(p) => unwrap_expr(&p.expr),
        Expr::Group(g) => unwrap_expr(&g.expr),
        Expr::Reference(r) => unwrap_expr(&r.expr),
        other => other,
    }
}

const UNSUPPORTED_ARG: &str = "unsupported argument in a #[workflow] call. \
Allowed: a literal, a workflow input parameter, a binding from a prior \
`let x = template(...);`, or `.to_string()`/`.to_owned()`/`.into()` on one \
of those. Computed values, regular variables/consts, method calls, and \
other expressions aren't lowered yet.";

/// Strict: a literal, a previous-task reference, or a workflow-input
/// reference. Anything else is a hard `compile_error!` (no silent
/// stringify) — an unmodeled arg would otherwise emit a bogus Argo param.
fn expr_to_arg(
    e: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<Arg> {
    match unwrap_expr(e) {
        Expr::Lit(syn::ExprLit { lit, .. }) => Ok(match lit {
            syn::Lit::Str(s) => Arg::Lit(s.value()),
            syn::Lit::Int(i) => Arg::Lit(i.base10_digits().to_string()),
            syn::Lit::Float(f) => Arg::Lit(f.base10_digits().to_string()),
            syn::Lit::Bool(b) => Arg::Lit(b.value.to_string()),
            other => Arg::Lit(quote!(#other).to_string()),
        }),
        // `<x>.to_string()/.to_owned()/.into()` — only if the receiver is
        // itself a supported arg (literal / input / prior binding).
        Expr::MethodCall(mc)
            if matches!(
                mc.method.to_string().as_str(),
                "to_string" | "to_owned" | "into"
            ) && mc.args.is_empty() =>
        {
            expr_to_arg(&mc.receiver, bindings, inputs)
        }
        Expr::Path(p) if p.path.segments.len() == 1 => {
            let name = p.path.segments[0].ident.to_string();
            if let Some(task) = bindings.get(&name) {
                Ok(Arg::Ref(task.clone()))
            } else if inputs.contains(&name) {
                Ok(Arg::Input(name))
            } else {
                Err(syn::Error::new_spanned(
                    e,
                    format!(
                        "`{name}` is not a #[workflow] input or a binding from \
                         a prior `let = template(...)`. {UNSUPPORTED_ARG}"
                    ),
                ))
            }
        }
        other => Err(syn::Error::new_spanned(other, UNSUPPORTED_ARG)),
    }
}

/// A call `path(args...)` where `path` is any path expression. Returns the
/// full callee path (so cross-crate `foo::ingest(..)` works) and its args.
fn call_parts(e: &Expr) -> Option<(Path, Vec<Expr>)> {
    if let Expr::Call(c) = unwrap_expr(e)
        && let Expr::Path(p) = &*c.func
    {
        return Some((p.path.clone(), c.args.iter().cloned().collect()));
    }
    None
}

/// A bare path expression → its `Path` (a template identity).
fn expr_path(e: &Expr) -> Option<Path> {
    match unwrap_expr(e) {
        Expr::Path(p) => Some(p.path.clone()),
        _ => None,
    }
}

/// Peel trailing builder method calls (`.continue_on`/`.hooks`/`.on_exit`)
/// off `e`, accumulating a `NodeOpts`, and return the inner base
/// expression (which the caller still validates is a template call). An
/// unknown trailing method is *not* consumed — left for the caller's
/// normal not-a-template-call diagnostic — but a malformed *known*
/// builder is a hard, targeted `compile_error!`.
fn peel_builders(e: &Expr) -> syn::Result<(&Expr, NodeOpts)> {
    let mut opts = NodeOpts::default();
    let mut on_exit_seen = false;
    let mut cur = e;
    while let Expr::MethodCall(mc) = unwrap_expr(cur) {
        match mc.method.to_string().as_str() {
            "continue_on" => {
                if opts.continue_on.is_some() {
                    return Err(syn::Error::new_spanned(
                        mc, "`.continue_on(...)` specified more than once.",
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
                        mc, "`.on_exit(...)` specified more than once.",
                    ));
                }
                on_exit_seen = true;
                let mut it = mc.args.iter();
                let (Some(arg), None) = (it.next(), it.next()) else {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.on_exit(t)` / `.on_exit(t(args))` takes exactly \
                         one template (optionally called with args).",
                    ));
                };
                // `t` (bare path) OR `t(arg, …)` (call with args).
                let (template, raw_args) = if let Some((p, raw)) =
                    call_parts(arg)
                {
                    (p, raw)
                } else if let Some(p) = expr_path(arg) {
                    (p, Vec::new())
                } else {
                    return Err(syn::Error::new_spanned(
                        arg,
                        "`.on_exit(t)`/`.on_exit(t(args))`: `t` must be a \
                         template path.",
                    ));
                };
                opts.hooks.push(HookSpec {
                    expression: None,
                    template,
                    raw_args,
                });
            }
            "hooks" => {
                if mc.args.is_empty() {
                    return Err(syn::Error::new_spanned(
                        mc,
                        "`.hooks(...)` needs at least one \
                         `\"argo-expression\" = template` entry.",
                    ));
                }
                for a in &mc.args {
                    let Expr::Assign(asn) = unwrap_expr(a) else {
                        return Err(syn::Error::new_spanned(
                            a,
                            "each `.hooks(...)` entry must be \
                             `\"argo-expression\" = template`.",
                        ));
                    };
                    let expression = match unwrap_expr(&asn.left) {
                        Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(s), ..
                        }) => s.value(),
                        _ => {
                            return Err(syn::Error::new_spanned(
                                &asn.left,
                                "hook key must be a string-literal Argo \
                                 expression.",
                            ));
                        }
                    };
                    // `.hooks(...)` arg-grammar is deferred — value must
                    // be a bare template path for now.
                    let template = expr_path(&asn.right).ok_or_else(|| {
                        syn::Error::new_spanned(
                            &asn.right,
                            "hook value must be a bare template path \
                             (passing args to `.hooks(...)` targets isn't \
                             supported yet — use `.on_exit(t(args))`).",
                        )
                    })?;
                    opts.hooks.push(HookSpec {
                        expression: Some(expression),
                        template,
                        raw_args: Vec::new(),
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
fn path_leaf(p: &Path) -> String {
    p.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_else(|| "task".to_string())
}

const UNSUPPORTED_STMT: &str = "unsupported statement in a #[workflow]: only \
`let x = template(args);` and `template(args);` are lowered. if/match, \
for/while/loop, macros, method calls and other expressions aren't \
supported yet — they'll be lowered differently later.";

const NOT_A_TEMPLATE_CALL: &str = "expected a template call `name(args)` — a \
#[container] or #[workflow]. #[fragment]s and regular functions are not \
templates and can't be called from a #[workflow].";

const RETURN_UNRESOLVED: &str = "this #[workflow] declares a return type but \
its returned value isn't produced by a template call. End with a tail \
template call `name(args)` (no `;`) or return a `let` binding from one.";

/// The binding name a `let` pattern introduces: `Some(name)` for a plain
/// (optionally `mut`/typed) ident, `None` for `_`. Anything else (tuple,
/// ref, struct, or-pattern) is unsupported in a #[workflow].
fn local_binding(pat: &syn::Pat) -> syn::Result<Option<String>> {
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

/// Allocate a kebab task name unique within the workflow.
fn uniq_task(used: &mut std::collections::HashSet<String>, base: &str) -> String {
    let mut task = kebab(base);
    let mut n = 1;
    while used.contains(&task) {
        n += 1;
        task = format!("{}-{n}", kebab(base));
    }
    used.insert(task.clone());
    task
}

/// Returns `(nodes, Some(output_task))` where `output_task` is the task
/// whose `result` the workflow returns (only when it declares a return
/// type). Every statement is strictly matched: anything unmodeled is a
/// hard `compile_error!` rather than a silently dropped node.
fn analyze_workflow(func: &ItemFn) -> syn::Result<(Vec<Node>, Option<String>)> {
    let mut bindings: std::collections::HashMap<String, String> = Default::default();
    let mut used: std::collections::HashSet<String> = Default::default();
    let inputs: std::collections::HashSet<String> = fn_args(func)
        .iter()
        .map(|(i, _)| i.to_string())
        .collect();
    let want_output = matches!(func.sig.output, syn::ReturnType::Type(..));
    let mut nodes: Vec<Node> = Vec::new();
    let mut output_task: Option<String> = None;

    // Record a `callee(raw...)` call (+ its peeled builder opts) as a
    // node; returns its task name.
    let push_call = |callee: Path,
                         raw: Vec<Expr>,
                         base: &str,
                         opts: NodeOpts,
                         used: &mut std::collections::HashSet<String>,
                         nodes: &mut Vec<Node>,
                         bindings: &std::collections::HashMap<String, String>|
     -> syn::Result<String> {
        let task = uniq_task(used, base);
        let args = raw
            .iter()
            .map(|a| expr_to_arg(a, bindings, &inputs))
            .collect::<syn::Result<Vec<_>>>()?;
        // Resolve each hook's raw args against the same binding/input
        // scope as the task's own args.
        let hooks = opts
            .hooks
            .into_iter()
            .map(|h| {
                let args = h
                    .raw_args
                    .iter()
                    .map(|a| expr_to_arg(a, bindings, &inputs))
                    .collect::<syn::Result<Vec<_>>>()?;
                Ok(Hook {
                    expression: h.expression,
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
        });
        Ok(task)
    };

    let stmts = &func.block.stmts;
    for (idx, stmt) in stmts.iter().enumerate() {
        let is_last = idx + 1 == stmts.len();
        match stmt {
            Stmt::Local(local) => {
                let bind = local_binding(&local.pat)?;
                let init = local.init.as_ref().ok_or_else(|| {
                    syn::Error::new_spanned(
                        local,
                        "a #[workflow] `let` must bind a template call: \
                         `let x = template(args);`.",
                    )
                })?;
                if init.diverge.is_some() {
                    return Err(syn::Error::new_spanned(
                        local,
                        "`let ... else` is not supported in #[workflow].",
                    ));
                }
                let (base_expr, opts) = peel_builders(&init.expr)?;
                let (callee, raw) = call_parts(base_expr).ok_or_else(|| {
                    syn::Error::new_spanned(&init.expr, NOT_A_TEMPLATE_CALL)
                })?;
                let base = bind.clone().unwrap_or_else(|| path_leaf(&callee));
                let task = push_call(
                    callee, raw, &base, opts, &mut used, &mut nodes, &bindings,
                )?;
                if let Some(b) = bind {
                    bindings.insert(b, task);
                }
            }
            Stmt::Expr(expr, semi) => {
                if let Expr::Return(r) = unwrap_expr(expr) {
                    let target = r.expr.as_deref().ok_or_else(|| {
                        syn::Error::new_spanned(
                            expr,
                            "#[workflow] `return` must return a template result.",
                        )
                    })?;
                    let (base_expr, opts) = peel_builders(target)?;
                    output_task = Some(match unwrap_expr(base_expr) {
                        Expr::Path(p) if p.path.segments.len() == 1 => {
                            if opts.continue_on.is_some()
                                || !opts.hooks.is_empty()
                            {
                                return Err(syn::Error::new_spanned(
                                    target,
                                    "`.continue_on`/`.hooks`/`.on_exit` must \
                                     be chained on a template call, not a \
                                     returned binding.",
                                ));
                            }
                            let name = p.path.segments[0].ident.to_string();
                            bindings.get(&name).cloned().ok_or_else(|| {
                                syn::Error::new_spanned(
                                    target,
                                    format!(
                                        "`{name}` is returned but isn't a \
                                         binding from a `let = template(...)`."
                                    ),
                                )
                            })?
                        }
                        _ => {
                            let (callee, raw) =
                                call_parts(base_expr).ok_or_else(|| {
                                    syn::Error::new_spanned(
                                        target,
                                        NOT_A_TEMPLATE_CALL,
                                    )
                                })?;
                            let base = path_leaf(&callee);
                            push_call(
                                callee, raw, &base, opts, &mut used,
                                &mut nodes, &bindings,
                            )?
                        }
                    });
                } else if let Expr::Path(p) = unwrap_expr(expr) {
                    // tail bare binding ident == the returned value
                    if !(is_last
                        && semi.is_none()
                        && p.path.segments.len() == 1)
                    {
                        return Err(syn::Error::new_spanned(
                            expr, UNSUPPORTED_STMT,
                        ));
                    }
                    let name = p.path.segments[0].ident.to_string();
                    output_task = Some(
                        bindings.get(&name).cloned().ok_or_else(|| {
                            syn::Error::new_spanned(
                                expr,
                                format!(
                                    "`{name}` is returned but isn't a binding \
                                     from a `let = template(...)`."
                                ),
                            )
                        })?,
                    );
                } else {
                    // A (possibly builder-chained) call statement / tail
                    // call: peel `.continue_on`/`.hooks`/`.on_exit`, then
                    // the base must be a template call.
                    let (base_expr, opts) = peel_builders(expr)?;
                    let (callee, raw) =
                        call_parts(base_expr).ok_or_else(|| {
                            syn::Error::new_spanned(expr, UNSUPPORTED_STMT)
                        })?;
                    let base = path_leaf(&callee);
                    let task = push_call(
                        callee, raw, &base, opts, &mut used, &mut nodes,
                        &bindings,
                    )?;
                    if is_last && semi.is_none() && want_output {
                        output_task = Some(task);
                    }
                }
            }
            Stmt::Macro(m) => {
                return Err(syn::Error::new_spanned(m, UNSUPPORTED_STMT));
            }
            Stmt::Item(it) => {
                return Err(syn::Error::new_spanned(it, UNSUPPORTED_STMT));
            }
        }
    }

    if want_output && output_task.is_none() {
        return Err(syn::Error::new_spanned(&func.sig.output, RETURN_UNRESOLVED));
    }
    if !want_output {
        output_task = None;
    }
    Ok((nodes, output_task))
}

/// `steps`: emit an Argo `steps` group (sequential, refs via
/// `{{steps.X…}}`, no `dependencies`) instead of a `dag` task.
fn node_tokens(node: &Node, steps: bool) -> TokenStream2 {
    let task = &node.task;
    let callee = &node.callee;
    let ref_scope = if steps { "{{steps." } else { "{{tasks." };

    let arg_stmts = node.args.iter().enumerate().map(|(i, a)| match a {
        Arg::Lit(v) => quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(#v.to_string()),
                    ..::core::default::Default::default()
                });
            }
        },
        Arg::Ref(dep) => {
            let dep_push = if steps {
                quote! {}
            } else {
                quote! { __deps.push(#dep.to_string()); }
            };
            quote! {
                {
                    let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                    #dep_push
                    let mut __v = ::std::string::String::from(#ref_scope);
                    __v.push_str(#dep);
                    // `outputs.parameters.return` — the explicitly declared
                    // param. NOT `outputs.result` (Argo's script-stdout
                    // alias: only exists for container/script tmpls, never
                    // dag/steps, so a sub-workflow's return needs this).
                    __v.push_str(".outputs.parameters.return}}");
                    __params.push(::cargo_athena::api::Parameter {
                        name: __name,
                        value: ::core::option::Option::Some(__v),
                        ..::core::default::Default::default()
                    });
                }
            }
        }
        Arg::Input(name) => quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                let mut __v = ::std::string::String::from("{{inputs.parameters.");
                __v.push_str(#name);
                __v.push_str("}}");
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(__v),
                    ..::core::default::Default::default()
                });
            }
        },
    });

    let push = if steps {
        quote! {
            // one sequential step group containing this single step
            __steps.push(::std::vec![__step]);
        }
    } else {
        quote! { __tasks.push(__step); }
    };

    // `.continue_on(...)` -> Argo continueOn.
    let continue_on_tok = match node.continue_on {
        Some((err, failed)) => quote! {
            ::core::option::Option::Some(::cargo_athena::api::ContinueOn {
                error: #err,
                failed: #failed,
            })
        },
        None => quote! { ::core::option::Option::None },
    };

    // `.on_exit(t)`/`.on_exit(t(args))` -> key `exit` (no expression);
    // `.hooks("e" = t, …)` -> keys hook1, hook2, … (source order). Hook
    // templates resolve their Argo name + INPUTS via the wormhole, like
    // callees; hook args use the same scope as the task's own args
    // (literal / workflow input / prior binding), but add NO dependency.
    let mut hook_n: u32 = 0;
    let hook_inserts: Vec<TokenStream2> = node
        .hooks
        .iter()
        .map(|h| {
            let hp = &h.template;
            let (key, expr_lit) = match &h.expression {
                None => ("exit".to_string(), String::new()),
                Some(e) => {
                    hook_n += 1;
                    (format!("hook{hook_n}"), e.clone())
                }
            };
            let arg_pushes = h.args.iter().enumerate().map(|(i, a)| match a {
                Arg::Lit(v) => quote! {
                    __hp.push(::cargo_athena::api::Parameter {
                        name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                        value: ::core::option::Option::Some(#v.to_string()),
                        ..::core::default::Default::default()
                    });
                },
                Arg::Ref(dep) => quote! {
                    {
                        let mut __v = ::std::string::String::from(#ref_scope);
                        __v.push_str(#dep);
                        __v.push_str(".outputs.parameters.return}}");
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(__v),
                            ..::core::default::Default::default()
                        });
                    }
                },
                Arg::Input(name) => quote! {
                    {
                        let mut __v =
                            ::std::string::String::from("{{inputs.parameters.");
                        __v.push_str(#name);
                        __v.push_str("}}");
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(__v),
                            ..::core::default::Default::default()
                        });
                    }
                },
            });
            quote! {
                {
                    let __hn =
                        <#hp as ::cargo_athena::Template>::ARGO_NAME;
                    let __hin: &[&str] =
                        <#hp as ::cargo_athena::Template>::INPUTS;
                    let mut __hp: ::std::vec::Vec<
                        ::cargo_athena::api::Parameter,
                    > = ::std::vec::Vec::new();
                    #( #arg_pushes )*
                    let __hargs = if __hp.is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            ::cargo_athena::api::Arguments {
                                parameters: __hp,
                                ..::core::default::Default::default()
                            },
                        )
                    };
                    __hooks.insert(
                        #key.to_string(),
                        ::cargo_athena::api::LifecycleHook {
                            template_ref: ::core::option::Option::Some(
                                ::cargo_athena::api::TemplateRef {
                                    name: __hn.to_string(),
                                    template: __hn.to_string(),
                                    cluster_scope: false,
                                },
                            ),
                            arguments: __hargs,
                            expression: #expr_lit.to_string(),
                        },
                    );
                }
            }
        })
        .collect();

    quote! {
        {
            // Resolved by the type system across modules/crates:
            let __ref_name =
                <#callee as ::cargo_athena::Template>::ARGO_NAME;
            let __inputs: &[&str] =
                <#callee as ::cargo_athena::Template>::INPUTS;
            let mut __params: ::std::vec::Vec<::cargo_athena::api::Parameter> =
                ::std::vec::Vec::new();
            let mut __deps: ::std::vec::Vec<::std::string::String> =
                ::std::vec::Vec::new();
            #( #arg_stmts )*
            __deps.sort();
            __deps.dedup();
            let __continue_on = #continue_on_tok;
            let mut __hooks: ::std::collections::BTreeMap<
                ::std::string::String,
                ::cargo_athena::api::LifecycleHook,
            > = ::std::collections::BTreeMap::new();
            #( #hook_inserts )*
            let __step = ::cargo_athena::api::DagTask {
                name: #task.to_string(),
                template: ::std::string::String::new(),
                dependencies: __deps,
                arguments: ::core::option::Option::Some(::cargo_athena::api::Arguments {
                    parameters: __params,
                    ..::core::default::Default::default()
                }),
                template_ref: ::core::option::Option::Some(
                    ::cargo_athena::api::TemplateRef {
                        name: __ref_name.to_string(),
                        template: __ref_name.to_string(),
                        cluster_scope: false,
                    }
                ),
                continue_on: __continue_on,
                hooks: __hooks,
            };
            #push
        }
    }
}

#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let cfg: WorkflowArgs = match parse_attr(attr) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let steps_mode = cfg.steps.is_set();

    let vis = &func.vis;
    let ident = func.sig.ident.clone();
    let rust_name = ident.to_string();
    let argo_name = make_argo_name(&cfg.name, &rust_name);

    let scan = scan_body(&func);
    if !scan.host_paths.is_empty()
        || !scan.in_artifacts.is_empty()
        || !scan.out_artifacts.is_empty()
    {
        return syn::Error::new_spanned(
            &func.sig.ident,
            "`host!`/`load_artifact*!`/`save_artifact*!` cannot be used in a \
             #[workflow]: a workflow is a DAG, not a pod. Declare them in the \
             #[container] (or a #[fragment] it calls) that runs in-cluster.",
        )
        .to_compile_error()
        .into();
    }
    let (nodes, output_task) = match analyze_workflow(&func) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let node_blocks: Vec<_> = nodes
        .iter()
        .map(|n| node_tokens(n, steps_mode))
        .collect();

    // A returned value bubbles the terminal task's `return` up as this
    // template's own `outputs.parameters.return`, so a parent can wire
    // {{tasks.X.outputs.parameters.return}} to a sub-workflow exactly
    // like a container.
    let outputs_tokens = match &output_task {
        Some(t) => {
            let scope = if steps_mode { "steps" } else { "tasks" };
            let refstr =
                format!("{{{{{scope}.{t}.outputs.parameters.return}}}}");
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
        None => quote! {},
    };

    // Distinct callee + hook-template + on_exit paths -> recurse their
    // `collect` (force-links them so they're emitted like any callee).
    let mut seen_callees = std::collections::HashSet::new();
    let callee_paths: Vec<&Path> = nodes
        .iter()
        .flat_map(|n| {
            std::iter::once(&n.callee)
                .chain(n.hooks.iter().map(|h| &h.template))
        })
        .chain(cfg.on_exit.iter())
        .filter(|p| seen_callees.insert(quote!(#p).to_string()))
        .collect();

    let arg_names: Vec<String> = fn_args(&func)
        .iter()
        .map(|(i, _)| i.to_string())
        .collect();
    let inputs_slice = str_slice(&arg_names);
    let inputs_tokens = if arg_names.is_empty() {
        quote! { ::core::option::Option::None }
    } else {
        quote! {
            ::core::option::Option::Some(::cargo_athena::api::Inputs {
                parameters: ::std::vec![
                    #( ::cargo_athena::api::Parameter {
                        name: #arg_names.to_string(),
                        ..::core::default::Default::default()
                    } ),*
                ],
                ..::core::default::Default::default()
            })
        }
    };

    // Default = `dag:` (parallel by data-deps). `#[workflow(steps)]` =
    // `steps:` (one sequential group per statement).
    let build_body = if steps_mode {
        quote! {
            let mut __steps: ::std::vec::Vec<
                ::std::vec::Vec<::cargo_athena::api::DagTask>,
            > = ::std::vec::Vec::new();
            #( #node_blocks )*
            ::cargo_athena::api::Template {
                name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                inputs: #inputs_tokens,
                steps: __steps,
                #outputs_tokens
                ..::core::default::Default::default()
            }
        }
    } else {
        quote! {
            let mut __tasks: ::std::vec::Vec<::cargo_athena::api::DagTask> =
                ::std::vec::Vec::new();
            #( #node_blocks )*
            ::cargo_athena::api::Template {
                name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                inputs: #inputs_tokens,
                dag: ::core::option::Option::Some(
                    ::cargo_athena::api::DagTemplate { tasks: __tasks }),
                #outputs_tokens
                ..::core::default::Default::default()
            }
        }
    };

    // `#[workflow(on_exit = t)]` -> Template::ON_EXIT (only the emit
    // root's reaches a runnable Workflow's spec.onExit).
    let on_exit_const = match &cfg.on_exit {
        Some(p) => quote! {
            const ON_EXIT: ::core::option::Option<&'static str> =
                ::core::option::Option::Some(
                    <#p as ::cargo_athena::Template>::ARGO_NAME,
                );
        },
        None => quote! {},
    };

    let expanded = quote! {
        // The body is compiled to Argo; the public identity is a type.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo_name;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Workflow;
            #on_exit_const

            fn build(_ctx: &::cargo_athena::BuildCtx)
                -> ::cargo_athena::api::Template
            {
                #build_body
            }

            fn collect(__out: &mut ::cargo_athena::Collector) {
                if !__out.enter(<Self as ::cargo_athena::Template>::ARGO_NAME) {
                    return;
                }
                __out.add_builder(<Self as ::cargo_athena::Template>::build);
                #(
                    <#callee_paths as ::cargo_athena::Template>::collect(__out);
                )*
            }
        }
    };
    expanded.into()
}

// ---------------------------------------------------------------------------
// #[fragment]
// ---------------------------------------------------------------------------

#[proc_macro_attribute]
pub fn fragment(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let rust_name = func.sig.ident.to_string();
    let scan = scan_body(&func);
    let func = with_host_rewritten(&func);
    let host_slice = str_slice(&scan.host_paths);
    let in_art_slice = str_slice(&scan.in_artifacts);
    let out_art_slice = str_slice(&scan.out_artifacts);
    let callee_slice = str_slice(&scan.callees);

    let expanded = quote! {
        #func

        ::cargo_athena::inventory::submit! {
            ::cargo_athena::FragmentReg {
                rust_name: #rust_name,
                host_paths: #host_slice,
                in_artifacts: #in_art_slice,
                out_artifacts: #out_art_slice,
                callees: #callee_slice,
            }
        }
    };
    expanded.into()
}
