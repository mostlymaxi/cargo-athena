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

/// A never-run signature shim with the fn's *real* arg + return types,
/// as an inherent fn on the identity struct. Lets the `#[workflow]` ghost
/// type-check data flow (`Callee::__athena_sig(args)`) while the public
/// identity stays a unit struct (the wormhole). `pub` + `#[doc(hidden)]`
/// so it resolves cross-crate/module exactly like the type path.
fn sig_shim(ident: &syn::Ident, func: &ItemFn) -> TokenStream2 {
    let inputs = &func.sig.inputs;
    let output = &func.sig.output;
    quote! {
        impl #ident {
            #[doc(hidden)]
            #[allow(dead_code, unused_variables, clippy::all)]
            pub fn __athena_sig(#inputs) #output {
                ::core::unimplemented!("athena ghost: never executed")
            }
        }
    }
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

/// YAML 1.1 parsers (Argo's Go YAML→JSON among them) read certain bare
/// words as bool/null even though YAML 1.2 / serde_norway treat them as
/// strings — the classic "Norway problem". Returns how `s` would be
/// mis-typed, or `None` if it is safe. (Rust identifiers can't be the
/// numeric/sexagesimal forms, so only the word set can ever fire.)
fn yaml_ambiguous(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "y" | "yes" | "n" | "no" | "true" | "false" | "on" | "off" => {
            Some("a YAML 1.1 boolean")
        }
        "null" | "~" => Some("a YAML 1.1 null"),
        _ => None,
    }
}

/// Reject argument names (→ Argo parameter names) and `name = "…"`
/// overrides that a YAML 1.1 parser would silently mis-type. Spanned at
/// the offending token so the fix is obvious. Synthetic `if`/`else`
/// wrappers reuse the `#[workflow]` codegen path, so their captured
/// inputs are covered by the same check.
fn check_yaml_safe_names(
    func: &ItemFn,
    name_override: &Option<String>,
) -> Result<(), TokenStream> {
    for (ident, _) in fn_args(func) {
        if let Some(why) = yaml_ambiguous(&ident.to_string()) {
            let n = ident.to_string();
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "Argo parameter name `{n}` is unsafe: a YAML 1.1 \
                     parser (including Argo's Go YAML→JSON) reads the bare \
                     word `{n}` as {why}, not a string — likewise \
                     y/yes/n/no/on/off/true/false (any case) and null/~. \
                     The emitted workflow would be silently mis-typed. \
                     Rename this argument."
                ),
            )
            .to_compile_error()
            .into());
        }
    }
    if let Some(n) = name_override
        && let Some(why) = yaml_ambiguous(n)
    {
        return Err(syn::Error::new(
            func.sig.ident.span(),
            format!(
                "`name = \"{n}\"` is unsafe: a YAML 1.1 parser \
                 (including Argo's) reads `{n}` as {why}, not a \
                 string. Choose a different Argo name."
            ),
        )
        .to_compile_error()
        .into());
    }
    Ok(())
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

/// The builder methods peeled off task calls (kept in sync with
/// `peel_builders`). Stripped from the `#[workflow]` ghost since they
/// aren't real methods on the values.
fn is_builder_method(m: &syn::Ident) -> bool {
    matches!(
        m.to_string().as_str(),
        "continue_on"
            | "on_exit"
            | "on_success"
            | "on_failure"
            | "on_error"
            | "hook_if"
    )
}

/// Rewrites a clone of a `#[workflow]` body into the never-run ghost:
/// strip builder chains (→ their receiver) and rewrite every template
/// call `C(args)` → `C::__athena_sig(args)`. The result is faithful Rust
/// (move semantics intact — fan-out needs an explicit `.clone()`, which
/// mirrors Argo copying the output param into each consumer), so rustc
/// enforces arg/field/return types on the analyzed body.
struct GhostRewrite;

impl VisitMut for GhostRewrite {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        while let Expr::MethodCall(mc) = e {
            if is_builder_method(&mc.method) {
                let recv = (*mc.receiver).clone();
                *e = recv;
            } else {
                break;
            }
        }
        if let Expr::Call(c) = e
            && let Expr::Path(p) = &mut *c.func
        {
            p.path
                .segments
                .push(syn::PathSegment::from(format_ident!("__athena_sig")));
        }
        syn::visit_mut::visit_expr_mut(self, e);
    }
}

/// Build the hidden, never-called type-check ghost for a `#[workflow]`.
fn ghost_fn(func: &ItemFn) -> TokenStream2 {
    let mut block = func.block.clone();
    GhostRewrite.visit_block_mut(&mut block);
    let name = format_ident!("__athena_tc_{}", func.sig.ident);
    let inputs = &func.sig.inputs;
    let output = &func.sig.output;
    quote! {
        #[doc(hidden)]
        #[allow(dead_code, unused, clippy::all)]
        fn #name(#inputs) #output {
            // In scope so `list.fan_out(|x| C::__athena_sig(x, ..))`
            // type-checks (element type, closure, resulting `Vec<U>`).
            #[allow(unused_imports)]
            use ::cargo_athena::AthenaList;
            #block
        }
    }
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
    if let Err(e) = check_yaml_safe_names(&func, &cfg.name) {
        return e;
    }

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
    let sig_block = sig_shim(&ident, &func);

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

        // Never-run typed signature shim (lets the #[workflow] ghost
        // type-check data flow through this template).
        #sig_block

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
    /// Named-field access into a serde value: `a.b.c` lowered via Argo
    /// expr-templating `{{=toJSON(fromJSON(<src>)['b']['c'])}}` (the
    /// universal-safe form — see `node_tokens`). The ghost has already
    /// type-checked that the field path is valid on the producer's type.
    Json { src: JsonSrc, path: Vec<String> },
    /// The fan-out closure parameter: `|x| C(x)` → `{{item}}`,
    /// `|x| C(x.f)` → `{{item.f}}` (only valid on a `fan_out` node).
    Item { path: Vec<String> },
}

/// Where a `Json` arg's root value comes from.
enum JsonSrc {
    /// A prior `let` binding → the producing task (adds a DAG dep).
    Task(String),
    /// A `#[workflow]` input parameter (no dep).
    Input(String),
}

/// The list a `fan_out` iterates (Argo `withParam`).
enum FanSrc {
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
enum HookWhen {
    Exit,
    Success,
    Failure,
    Error,
    /// `.hook_if("raw-argo-expression" = t)` — verbatim Argo expr.
    Raw(String),
}

/// A hook peeled off a call, args still as raw `Expr` (resolved against
/// bindings/inputs later in `push_call`).
struct HookSpec {
    when: HookWhen,
    /// Hook template path — force-linked + emitted as a `templateRef`
    /// exactly like a callee.
    template: Path,
    /// Args to the hook template (`t(args)`); empty for a bare path.
    raw_args: Vec<Expr>,
}

/// Per-task builders peeled off a call: `.continue_on(...)`, `.on_exit`,
/// `.on_success`/`.on_failure`/`.on_error`, `.hook_if(...)`.
#[derive(Default)]
struct NodeOpts {
    /// `(error, failed)` for Argo `continueOn`.
    continue_on: Option<(bool, bool)>,
    hooks: Vec<HookSpec>,
}

/// A hook with its args resolved to `Arg`s (post `push_call`).
struct Hook {
    when: HookWhen,
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
    /// `Some` ⇒ this is a `fan_out` task (Argo `withParam` over the
    /// source list; the callee runs once per `{{item}}`).
    fan: Option<FanSrc>,
    /// `Some` ⇒ a fully-rendered Argo `when` expression (the task runs
    /// only if it holds). Set on the arm tasks of a synthesized `if`
    /// wrapper; `None` for ordinary unconditional tasks.
    when: Option<String>,
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
`let x = template(...);`, `.clone()`/`.to_owned()` on a binding/input, or \
`.to_string()`/`.into()` on a string literal. Computed values, regular \
variables/consts, other method calls, and other expressions aren't \
lowered yet.";

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
        // Owned-value conversions, emitted identically to the receiver:
        //  * `.clone()`/`.to_owned()` are type-preserving → allowed on
        //    any supported receiver. `.clone()` is the explicit fan-out
        //    marker (Argo copies the output param into each consumer); it
        //    vanishes in the emit.
        //  * `.to_string()`/`.into()` can CHANGE the type, but the emit
        //    only ever passes the receiver's raw serialized param — so on
        //    a binding/input that would mismatch the ghost's type and
        //    break the (de)serialize contract. Restrict them to a string
        //    literal (where they're an identity for the emit).
        Expr::MethodCall(mc) if mc.args.is_empty() => {
            match mc.method.to_string().as_str() {
                "clone" | "to_owned" => {
                    expr_to_arg(&mc.receiver, bindings, inputs)
                }
                "to_string" | "into" => {
                    if matches!(
                        unwrap_expr(&mc.receiver),
                        Expr::Lit(syn::ExprLit {
                            lit: syn::Lit::Str(_),
                            ..
                        })
                    ) {
                        expr_to_arg(&mc.receiver, bindings, inputs)
                    } else {
                        Err(syn::Error::new_spanned(
                            mc,
                            "`.to_string()`/`.into()` in a #[workflow] arg \
                             is only allowed on a string literal. On a \
                             binding/input the value already has the right \
                             serialized type; converting it would mismatch \
                             the emitted Argo parameter (the raw serialized \
                             value) and break the (de)serialize contract — \
                             pass it directly, or `.clone()` to fan out.",
                        ))
                    }
                }
                _ => Err(syn::Error::new_spanned(mc, UNSUPPORTED_ARG)),
            }
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
            if p.path.segments.len() != 1 {
                return Err(syn::Error::new_spanned(cur, UNSUPPORTED_ARG));
            }
            let name = p.path.segments[0].ident.to_string();
            let src = if let Some(task) = bindings.get(&name) {
                JsonSrc::Task(task.clone())
            } else if inputs.contains(&name) {
                JsonSrc::Input(name)
            } else {
                return Err(syn::Error::new_spanned(
                    cur,
                    format!(
                        "`{name}` is not a #[workflow] input or a prior \
                         `let = template(...)` binding. {UNSUPPORTED_ARG}"
                    ),
                ));
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

/// Like `expr_to_arg`, but inside a `fan_out` closure `|it| …`: an
/// expression rooted at the item param `it` (optionally `.clone()`/
/// `.to_owned()`-wrapped, with named `.field`s) becomes `Arg::Item`
/// (`{{item}}`/`{{item.f}}`); anything else resolves normally.
fn resolve_arg(
    e: &Expr,
    item: Option<&str>,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<Arg> {
    if let Some(it) = item {
        let mut cur = unwrap_expr(e);
        while let Expr::MethodCall(mc) = cur {
            if mc.args.is_empty()
                && matches!(
                    mc.method.to_string().as_str(),
                    "clone" | "to_owned"
                )
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
    expr_to_arg(e, bindings, inputs)
}

/// The tail expression of a closure body (`|x| C(x)` or `|x| { …; C(x) }`).
fn closure_tail(b: &Expr) -> &Expr {
    if let Expr::Block(eb) = b
        && let Some(Stmt::Expr(e, _)) = eb.block.stmts.last()
    {
        return e;
    }
    b
}

/// `<list>.fan_out(|item| C(args))` → `(receiver, item-name, C, C-args)`.
/// `None` if `e` isn't that exact shape.
fn fan_parts(e: &Expr) -> Option<(Expr, String, Path, Vec<Expr>)> {
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
fn fan_src(
    recv: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> syn::Result<FanSrc> {
    let Expr::Path(p) = unwrap_expr(recv) else {
        return Err(syn::Error::new_spanned(
            recv,
            "`fan_out` source must be a prior `let` binding or a \
             #[workflow] input (a list).",
        ));
    };
    if p.path.segments.len() == 1 {
        let name = p.path.segments[0].ident.to_string();
        if let Some(task) = bindings.get(&name) {
            return Ok(FanSrc::Task(task.clone()));
        } else if inputs.contains(&name) {
            return Ok(FanSrc::Input(name));
        }
    }
    Err(syn::Error::new_spanned(
        recv,
        "`fan_out` source must be a prior `let` binding or a #[workflow] \
         input (a list).",
    ))
}

// ---------------------------------------------------------------------------
// `if` conditions -> Argo `when` (closed AST, valid-by-construction render)
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum CmpOp {
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
/// on real Argo v4.0.5). `Ref`/`Input`/`Json` are *parent-scoped*; the
/// `if` synthesis remaps them to the cond-wrapper's own input params.
enum WhenOp {
    /// Prior `let` binding → producing task (a DAG dep at cond level).
    Ref(String),
    /// A parent `#[workflow]` input parameter.
    Input(String),
    /// `a.b.c` named-field access (same lowering as `Arg::Json`).
    Json { src: JsonSrc, path: Vec<String> },
    Str(String),
    Int(String),
    Bool(bool),
}

/// Closed, parenthesized-on-render condition AST. The single `render`
/// (in the `if` synthesis) is the only producer of a `when` string, so a
/// malformed Argo expression is unrepresentable.
enum WhenExpr {
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

const UNSUPPORTED_COND: &str = "unsupported `if` condition. Allowed: \
comparisons (== != < <= > >=) and `&&`/`||`/`!` over a #[workflow] input, \
a prior `let = template(...)` binding, an `a.field` of one, or a literal. \
Method calls, arithmetic, function calls and casts aren't lowered.";

/// Resolve a single condition operand, preserving literal kind. Reuses
/// the `expr_to_arg` field/binding/input rules so behaviour matches task
/// args exactly (incl. `.clone()`/`.to_owned()` passthrough).
fn cond_operand(
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
        Ok(Arg::Ref(t)) => Ok(WhenOp::Ref(t)),
        Ok(Arg::Input(n)) => Ok(WhenOp::Input(n)),
        Ok(Arg::Json { src, path }) => Ok(WhenOp::Json { src, path }),
        // `Arg::Lit` only comes back for a literal, handled above; `Item`
        // can't occur outside a fan_out closure.
        Ok(_) => Err(syn::Error::new_spanned(e, UNSUPPORTED_COND)),
        Err(_) => Err(syn::Error::new_spanned(e, UNSUPPORTED_COND)),
    }
}

/// Total lowering of a type-checked Rust condition into `WhenExpr`.
/// Anything outside the grammar is a spanned `compile_error!` — never a
/// mistranslation (consistent with the strict #[workflow] body contract).
fn cond_to_when(
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
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Not(_)) => Ok(
            WhenExpr::Not(Box::new(cond_to_when(&u.expr, bindings, inputs)?)),
        ),
        // A bare operand condition: `if flag` / `if a.enabled`.
        other => Ok(WhenExpr::Truthy(cond_operand(other, bindings, inputs)?)),
    }
}

/// A single condition operand: if it's a template call (`if foo() > 3`),
/// hoist it to a parent task (Rust evaluates the condition regardless of
/// branch, so it runs unconditionally) and substitute a reference to it;
/// identical calls within one `if` are hoisted once. Otherwise unchanged.
#[allow(clippy::too_many_arguments)]
fn hoist_operand(
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
fn hoist_cond(
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
                    hoist_operand(
                        &b.left, used, nodes, bindings, inputs, seen,
                        cond_binds,
                    )?,
                    hoist_operand(
                        &b.right, used, nodes, bindings, inputs, seen,
                        cond_binds,
                    )?,
                ),
                And(_) | Or(_) => (
                    hoist_cond(
                        &b.left, used, nodes, bindings, inputs, seen,
                        cond_binds,
                    )?,
                    hoist_cond(
                        &b.right, used, nodes, bindings, inputs, seen,
                        cond_binds,
                    )?,
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
        _ => hoist_operand(
            e, used, nodes, bindings, inputs, seen, cond_binds,
        ),
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

/// A hook target: `t` (bare path) or `t(arg, …)` (call with args).
fn hook_target(arg: &Expr) -> syn::Result<(Path, Vec<Expr>)> {
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
fn single_hook_target(
    mc: &syn::ExprMethodCall,
) -> syn::Result<(Path, Vec<Expr>)> {
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
                            lit: syn::Lit::Str(s), ..
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

/// What a (synthetic or real) workflow's `outputs.parameters.return`
/// resolves to.
enum SynthOut {
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
struct SynthWf {
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
fn referenced_idents(stmts: &[Stmt]) -> Vec<String> {
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
fn locally_bound(stmts: &[Stmt]) -> std::collections::HashSet<String> {
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
fn if_arms(mut e: &syn::ExprIf) -> Vec<(Option<Expr>, syn::Block)> {
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

fn when_op_str(o: &WhenOp) -> String {
    match o {
        WhenOp::Input(n) => format!("{{{{inputs.parameters.{n}}}}}"),
        WhenOp::Ref(t) => {
            format!("{{{{tasks.{t}.outputs.parameters.return}}}}")
        }
        WhenOp::Json { src, path } => {
            let acc: String =
                path.iter().map(|f| format!("['{f}']")).collect();
            let refexpr = match src {
                JsonSrc::Task(t) => {
                    format!("tasks['{t}'].outputs.parameters['return']")
                }
                JsonSrc::Input(n) => format!("inputs.parameters['{n}']"),
            };
            format!("{{{{=toJSON(fromJSON({refexpr}){acc})}}}}")
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
fn render_when(w: &WhenExpr) -> String {
    match w {
        WhenExpr::Cmp { lhs, op, rhs } => format!(
            "({} {} {})",
            when_op_str(lhs),
            op.argo(),
            when_op_str(rhs)
        ),
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
fn select_expr(arms: &[String]) -> String {
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
fn push_call(
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
fn callee_paths(nodes: &[Node], extra: &[Path]) -> Vec<Path> {
    let mut seen = std::collections::HashSet::new();
    nodes
        .iter()
        .flat_map(|n| {
            std::iter::once(&n.callee)
                .chain(n.hooks.iter().map(|h| &h.template))
        })
        .chain(extra.iter())
        .filter(|p| seen.insert(quote!(#p).to_string()))
        .cloned()
        .collect()
}

/// Lower an `if`/`else if`/`else` chain into one synthetic wrapper
/// workflow (+ a per-arm sub-workflow each). Pushes a single parent node
/// calling the wrapper (consumed exactly like a returning sub-workflow)
/// and returns its task name. `value` ⇒ the wrapper selects + returns
/// the taken arm's value.
#[allow(clippy::too_many_arguments)]
fn synth_if(
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
    let mut seen_calls: std::collections::HashMap<String, syn::Ident> =
        Default::default();
    let mut cond_binds: std::collections::HashMap<String, String> =
        Default::default();
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
                referenced_idents(std::slice::from_ref(&Stmt::Expr(
                    c.clone(),
                    None,
                ))),
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
    let cap_inputs: std::collections::HashSet<String> =
        captures.iter().cloned().collect();
    let empty_bindings = std::collections::HashMap::new();

    let k = ctx.if_ctr;
    ctx.if_ctr += 1;
    let wrap_ident =
        format_ident!("__athena_{}_if{}", ctx.parent_rust, k);
    let wrap_argo = format!("{}-if{}", ctx.parent_argo, k);

    // One sub-workflow + one when-gated wrapper task per arm.
    let mut wrap_nodes: Vec<Node> = Vec::new();
    let mut arm_tasks: Vec<String> = Vec::new();
    for (j, (_, body)) in arms.iter().enumerate() {
        let arm_ident =
            format_ident!("__athena_{}_if{}_arm{}", ctx.parent_rust, k, j);
        let arm_argo = format!("{wrap_argo}-arm{j}");
        let (anodes, aout) = analyze_stmts(
            &body.stmts,
            &cap_inputs,
            value,
            &arm_argo,
            &format!("{}_if{}_arm{}", ctx.parent_rust, k, j),
            ctx,
        )?;
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
                    WhenExpr::Not(Box::new(cond_to_when(
                        c,
                        &empty_bindings,
                        &cap_inputs,
                    )?)),
                );
            }
        }
        if let Some(c) = &arms[j].0 {
            conj(
                &mut gate,
                cond_to_when(c, &empty_bindings, &cap_inputs)?,
            );
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
struct SynthCtx {
    synth: Vec<SynthWf>,
    if_ctr: u32,
    parent_rust: String,
    parent_argo: String,
}

/// Analyze a statement slice into `(nodes, terminal_output_task)`. `if`
/// statements/initializers/tails are lowered via `synth_if`. Recursive
/// (arm bodies reuse this), so nested `if`s just work.
fn analyze_stmts(
    stmts: &[Stmt],
    inputs: &std::collections::HashSet<String>,
    want_output: bool,
    argo_self: &str,
    rust_self: &str,
    ctx: &mut SynthCtx,
) -> syn::Result<(Vec<Node>, Option<String>)> {
    let mut bindings: std::collections::HashMap<String, String> =
        Default::default();
    let mut used: std::collections::HashSet<String> = Default::default();
    let mut nodes: Vec<Node> = Vec::new();
    let mut output_task: Option<String> = None;

    // Recurse with this slice's identity as the synth parent name.
    let saved = (ctx.parent_rust.clone(), ctx.parent_argo.clone());
    ctx.parent_rust = rust_self.to_string();
    ctx.parent_argo = argo_self.to_string();

    for (idx, stmt) in stmts.iter().enumerate() {
        let is_last = idx + 1 == stmts.len();
        match stmt {
            Stmt::Local(local) => {
                let bind = local_binding(&local.pat)?;
                let init = local.init.as_ref().ok_or_else(|| {
                    syn::Error::new_spanned(
                        local,
                        "a #[workflow] `let` must bind a template call or \
                         `if`: `let x = template(args);`.",
                    )
                })?;
                if init.diverge.is_some() {
                    return Err(syn::Error::new_spanned(
                        local,
                        "`let ... else` is not supported in #[workflow].",
                    ));
                }
                let task = if let Expr::If(ei) = unwrap_expr(&init.expr) {
                    synth_if(
                        ei,
                        bind.as_deref(),
                        true,
                        &bindings,
                        inputs,
                        &mut used,
                        &mut nodes,
                        ctx,
                    )?
                } else {
                    let (base_expr, opts) = peel_builders(&init.expr)?;
                    if let Some((recv, item, callee, raw)) =
                        fan_parts(base_expr)
                    {
                        let fsrc = fan_src(&recv, &bindings, inputs)?;
                        let base = bind
                            .clone()
                            .unwrap_or_else(|| path_leaf(&callee));
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
                        let (callee, raw) = call_parts(base_expr)
                            .ok_or_else(|| {
                                syn::Error::new_spanned(
                                    &init.expr,
                                    NOT_A_TEMPLATE_CALL,
                                )
                            })?;
                        let base = bind
                            .clone()
                            .unwrap_or_else(|| path_leaf(&callee));
                        push_call(
                            callee,
                            raw,
                            &base,
                            opts,
                            None,
                            None,
                            &mut used,
                            &mut nodes,
                            &bindings,
                            inputs,
                        )?
                    }
                };
                if let Some(b) = bind {
                    bindings.insert(b, task);
                }
            }
            Stmt::Expr(expr, semi) => {
                if let Expr::If(ei) = unwrap_expr(expr) {
                    let value = is_last && semi.is_none() && want_output;
                    let task = synth_if(
                        ei,
                        None,
                        value,
                        &bindings,
                        inputs,
                        &mut used,
                        &mut nodes,
                        ctx,
                    )?;
                    if value {
                        output_task = Some(task);
                    }
                } else if let Expr::Return(r) = unwrap_expr(expr) {
                    let target = r.expr.as_deref().ok_or_else(|| {
                        syn::Error::new_spanned(
                            expr,
                            "#[workflow] `return` must return a template result.",
                        )
                    })?;
                    if let Expr::If(ei) = unwrap_expr(target) {
                        output_task = Some(synth_if(
                            ei,
                            None,
                            true,
                            &bindings,
                            inputs,
                            &mut used,
                            &mut nodes,
                            ctx,
                        )?);
                    } else {
                        let (base_expr, opts) = peel_builders(target)?;
                        output_task = Some(match unwrap_expr(base_expr) {
                            Expr::Path(p)
                                if p.path.segments.len() == 1 =>
                            {
                                if opts.continue_on.is_some()
                                    || !opts.hooks.is_empty()
                                {
                                    return Err(syn::Error::new_spanned(
                                        target,
                                        "`.continue_on`/`.hooks`/`.on_exit` \
                                         must be chained on a template \
                                         call, not a returned binding.",
                                    ));
                                }
                                let name =
                                    p.path.segments[0].ident.to_string();
                                bindings.get(&name).cloned().ok_or_else(
                                    || {
                                        syn::Error::new_spanned(
                                            target,
                                            format!(
                                                "`{name}` is returned but \
                                                 isn't a binding from a \
                                                 `let = template(...)`."
                                            ),
                                        )
                                    },
                                )?
                            }
                            _ => {
                                let (callee, raw) = call_parts(base_expr)
                                    .ok_or_else(|| {
                                        syn::Error::new_spanned(
                                            target,
                                            NOT_A_TEMPLATE_CALL,
                                        )
                                    })?;
                                let base = path_leaf(&callee);
                                push_call(
                                    callee,
                                    raw,
                                    &base,
                                    opts,
                                    None,
                                    None,
                                    &mut used,
                                    &mut nodes,
                                    &bindings,
                                    inputs,
                                )?
                            }
                        });
                    }
                } else if let Expr::Path(p) = unwrap_expr(expr) {
                    if !(is_last
                        && semi.is_none()
                        && p.path.segments.len() == 1)
                    {
                        return Err(syn::Error::new_spanned(
                            expr,
                            UNSUPPORTED_STMT,
                        ));
                    }
                    let name = p.path.segments[0].ident.to_string();
                    output_task = Some(
                        bindings.get(&name).cloned().ok_or_else(|| {
                            syn::Error::new_spanned(
                                expr,
                                format!(
                                    "`{name}` is returned but isn't a \
                                     binding from a `let = template(...)`."
                                ),
                            )
                        })?,
                    );
                } else {
                    let (base_expr, opts) = peel_builders(expr)?;
                    let task = if let Some((recv, item, callee, raw)) =
                        fan_parts(base_expr)
                    {
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
                        let (callee, raw) = call_parts(base_expr)
                            .ok_or_else(|| {
                                syn::Error::new_spanned(
                                    expr,
                                    UNSUPPORTED_STMT,
                                )
                            })?;
                        let base = path_leaf(&callee);
                        push_call(
                            callee,
                            raw,
                            &base,
                            opts,
                            None,
                            None,
                            &mut used,
                            &mut nodes,
                            &bindings,
                            inputs,
                        )?
                    };
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

    ctx.parent_rust = saved.0;
    ctx.parent_argo = saved.1;

    if want_output && output_task.is_none() {
        return Err(syn::Error::new_spanned(
            stmts.last().map(|s| quote!(#s)).unwrap_or_default(),
            RETURN_UNRESOLVED,
        ));
    }
    Ok((nodes, if want_output { output_task } else { None }))
}

/// Top-level: analyze a `#[workflow]` body, returning its nodes, terminal
/// output task, and every synthesized `if` wrapper/arm to also emit.
fn analyze_workflow(
    func: &ItemFn,
    parent_argo: &str,
) -> syn::Result<(Vec<Node>, Option<String>, Vec<SynthWf>)> {
    let inputs: std::collections::HashSet<String> = fn_args(func)
        .iter()
        .map(|(i, _)| i.to_string())
        .collect();
    let want_output = matches!(func.sig.output, syn::ReturnType::Type(..));
    let mut ctx = SynthCtx {
        synth: Vec::new(),
        if_ctr: 0,
        parent_rust: func.sig.ident.to_string(),
        parent_argo: parent_argo.to_string(),
    };
    let (nodes, output_task) = analyze_stmts(
        &func.block.stmts,
        &inputs,
        want_output,
        parent_argo,
        &func.sig.ident.to_string(),
        &mut ctx,
    )
    .map_err(|e| {
        // Re-target the generic "unresolved" span to the return type.
        if e.to_string() == RETURN_UNRESOLVED {
            syn::Error::new_spanned(&func.sig.output, RETURN_UNRESOLVED)
        } else {
            e
        }
    })?;
    Ok((nodes, output_task, ctx.synth))
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
        // `a.b.c` -> Argo expr-templating. toJSON(fromJSON(..)) is the
        // universal-safe round-trip (athena's run-side is `from_str` else
        // String, so it reconstructs every field type incl. quoted
        // strings & nested structs). Bracket form is hyphen/keyword-safe.
        Arg::Json { src, path } => {
            let scope = if steps { "steps" } else { "tasks" };
            let accessor: String =
                path.iter().map(|f| format!("['{f}']")).collect();
            let (refexpr, dep_push) = match src {
                JsonSrc::Task(dep) => {
                    let r = format!(
                        "{scope}['{dep}'].outputs.parameters['return']"
                    );
                    let dp = if steps {
                        quote! {}
                    } else {
                        quote! { __deps.push(#dep.to_string()); }
                    };
                    (r, dp)
                }
                JsonSrc::Input(name) => {
                    (format!("inputs.parameters['{name}']"), quote! {})
                }
            };
            let value =
                format!("{{{{=toJSON(fromJSON({refexpr}){accessor})}}}}");
            quote! {
                {
                    let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                    #dep_push
                    __params.push(::cargo_athena::api::Parameter {
                        name: __name,
                        value: ::core::option::Option::Some(#value.to_string()),
                        ..::core::default::Default::default()
                    });
                }
            }
        }
        // The fan-out closure param: `{{item}}` / `{{item.f.g}}`. Argo
        // binds `item` per iteration of this task's `withParam`.
        Arg::Item { path } => {
            let mut v = String::from("{{item");
            for f in path {
                v.push('.');
                v.push_str(f);
            }
            v.push_str("}}");
            quote! {
                __params.push(::cargo_athena::api::Parameter {
                    name: __inputs.get(#i).copied().unwrap_or_default().to_string(),
                    value: ::core::option::Option::Some(#v.to_string()),
                    ..::core::default::Default::default()
                });
            }
        }
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
    // Argo expression scope: a sibling node is `tasks['x']` in a dag,
    // `steps['x']` in a steps workflow. Bracket form (NOT `tasks.x`) so
    // hyphenated kebab task names resolve.
    let scope = if steps { "steps" } else { "tasks" };
    let hook_inserts: Vec<TokenStream2> = node
        .hooks
        .iter()
        .map(|h| {
            let hp = &h.template;
            let (key, expr_lit) = match &h.when {
                HookWhen::Exit => ("exit".to_string(), String::new()),
                when => {
                    hook_n += 1;
                    let expr = match when {
                        HookWhen::Success => format!(
                            "{scope}['{task}'].status == \"Succeeded\""
                        ),
                        HookWhen::Failure => format!(
                            "{scope}['{task}'].status == \"Failed\""
                        ),
                        HookWhen::Error => format!(
                            "{scope}['{task}'].status == \"Error\""
                        ),
                        HookWhen::Raw(s) => s.clone(),
                        HookWhen::Exit => unreachable!(),
                    };
                    (format!("hook{hook_n}"), expr)
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
                // Same expr-templating lowering as task args; hooks add
                // no DAG dependency.
                Arg::Json { src, path } => {
                    let s = if steps { "steps" } else { "tasks" };
                    let accessor: String =
                        path.iter().map(|f| format!("['{f}']")).collect();
                    let refexpr = match src {
                        JsonSrc::Task(dep) => format!(
                            "{s}['{dep}'].outputs.parameters['return']"
                        ),
                        JsonSrc::Input(name) => {
                            format!("inputs.parameters['{name}']")
                        }
                    };
                    let value = format!(
                        "{{{{=toJSON(fromJSON({refexpr}){accessor})}}}}"
                    );
                    quote! {
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(#value.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
                }
                Arg::Item { path } => {
                    let mut v = String::from("{{item");
                    for f in path {
                        v.push('.');
                        v.push_str(f);
                    }
                    v.push_str("}}");
                    quote! {
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(#v.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
                }
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

    // `fan_out` -> Argo `withParam` over the list source (+ a DAG dep on
    // the producing task when the source is a prior binding).
    let (with_param_val, fan_dep) = match &node.fan {
        Some(FanSrc::Task(dep)) => (
            format!("{ref_scope}{dep}.outputs.parameters.return}}}}"),
            if steps {
                quote! {}
            } else {
                quote! { __deps.push(#dep.to_string()); }
            },
        ),
        Some(FanSrc::Input(name)) => {
            (format!("{{{{inputs.parameters.{name}}}}}"), quote! {})
        }
        None => (String::new(), quote! {}),
    };

    // `if`-wrapper arm tasks carry a fully-rendered Argo `when`.
    let when_val = match &node.when {
        Some(w) => quote! { #w.to_string() },
        None => quote! { ::std::string::String::new() },
    };

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
            #fan_dep
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
                with_param: #with_param_val.to_string(),
                when: #when_val,
            };
            #push
        }
    }
}

/// Emit a synthesized `if` wrapper / arm as a hidden
/// `struct + impl Template` (Workflow-kind). No ghost / sig-shim / `run`
/// (never called from a Rust ghost, never runs in-pod); force-linked via
/// the parent's `collect` since the parent's `if` node names this type.
fn emit_synth(s: &SynthWf) -> TokenStream2 {
    let ident = &s.ident;
    let argo = &s.argo_name;
    let inputs_slice = str_slice(&s.inputs);
    let node_blocks: Vec<_> =
        s.nodes.iter().map(|n| node_tokens(n, false)).collect();
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
            let refstr =
                format!("{{{{tasks.{t}.outputs.parameters.return}}}}");
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
                if !__out.enter(
                    <Self as ::cargo_athena::Template>::ARGO_NAME,
                ) {
                    return;
                }
                __out.add_builder(
                    <Self as ::cargo_athena::Template>::build,
                );
                #(
                    <#callees as ::cargo_athena::Template>::collect(__out);
                )*
            }
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
    if let Err(e) = check_yaml_safe_names(&func, &cfg.name) {
        return e;
    }
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
    let (nodes, output_task, synths) =
        match analyze_workflow(&func, &argo_name) {
            Ok(v) => v,
            Err(e) => return e.to_compile_error().into(),
        };
    let node_blocks: Vec<_> = nodes
        .iter()
        .map(|n| node_tokens(n, steps_mode))
        .collect();
    // Synthesized `if` wrappers/arms, emitted flat as sibling items.
    let synth_items: Vec<TokenStream2> =
        synths.iter().map(emit_synth).collect();

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

    let sig_block = sig_shim(&ident, &func);
    let ghost = ghost_fn(&func);

    let expanded = quote! {
        // The body is compiled to Argo; the public identity is a type.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        // Typed signature shim + never-run ghost: rustc type-checks the
        // analyzed body's data flow (arg/field/return types) even though
        // the body itself isn't compiled.
        #sig_block
        #ghost

        // Synthesized `if` wrappers + arm sub-workflows (force-linked via
        // this workflow's `collect`, since its `if` nodes name them).
        #( #synth_items )*

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
