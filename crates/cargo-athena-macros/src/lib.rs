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
use syn::parse::Parser;
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

/// Parse an optional `name = "..."` from an attribute arg list.
fn parse_name_override(attr: &TokenStream2) -> Option<String> {
    if attr.is_empty() {
        return None;
    }
    let parser =
        syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated;
    let metas = parser
        .parse2(attr.clone())
        .expect("expected `key = \"value\"` arguments");
    for m in metas {
        if m.path.is_ident("name")
            && let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = m.value
        {
            return Some(s.value());
        }
    }
    None
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

struct ContainerArgs {
    /// Arbitrary, user-chosen runtime image (the point of `#[container]`).
    /// `None` => fall back to `[bootstrap].default_image` from athena.toml.
    image: Option<String>,
    name: Option<String>,
    /// `None` => fall back to `[defaults].service_account`.
    service_account: Option<String>,
}

fn parse_container_args(attr: TokenStream) -> ContainerArgs {
    let mut args = ContainerArgs {
        image: None,
        name: None,
        service_account: None,
    };
    if attr.is_empty() {
        return args;
    }
    let parser =
        syn::punctuated::Punctuated::<syn::MetaNameValue, syn::Token![,]>::parse_terminated;
    let metas = parser.parse(attr).expect("expected `key = \"value\"` arguments");
    for m in metas {
        let key = m.path.get_ident().map(|i| i.to_string()).unwrap_or_default();
        if let Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) = m.value
        {
            match key.as_str() {
                "image" => args.image = Some(s.value()),
                "name" => args.name = Some(s.value()),
                "service_account" => args.service_account = Some(s.value()),
                "bin" => panic!(
                    "#[container] no longer takes `bin`: the athena binary is \
                     delivered as an Argo artifact and exec'd by the bootstrap"
                ),
                other => panic!("unknown #[container] argument `{other}`"),
            }
        }
    }
    args
}

#[proc_macro_attribute]
pub fn container(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let cfg = parse_container_args(attr);

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
    let vis = &func.vis;

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
                    ::cargo_athena::artifact_inputs(&__in_names));
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
                            name: "result".to_string(),
                            value_from: ::core::option::Option::Some(
                                ::cargo_athena::api::ValueFrom {
                                    path: "/athena/result".to_string(),
                                    ..::core::default::Default::default()
                                }
                            ),
                            ..::core::default::Default::default()
                        } ],
                        artifacts: ::cargo_athena::artifact_outputs(&__out_names),
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

struct Node {
    task: String,
    /// Callee path exactly as written (`ingest`, `foo::ingest`, …) — used
    /// as a *type* in `<callee as Template>` so the compiler resolves its
    /// Argo name/inputs across modules and crates and force-links it.
    callee: Path,
    args: Vec<Arg>,
}

fn unwrap_expr(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(p) => unwrap_expr(&p.expr),
        Expr::Group(g) => unwrap_expr(&g.expr),
        Expr::Reference(r) => unwrap_expr(&r.expr),
        other => other,
    }
}

/// Best-effort: extract a literal string, a previous-task reference, or a
/// workflow-input reference.
fn expr_to_arg(
    e: &Expr,
    bindings: &std::collections::HashMap<String, String>,
    inputs: &std::collections::HashSet<String>,
) -> Arg {
    match unwrap_expr(e) {
        Expr::Lit(syn::ExprLit { lit, .. }) => match lit {
            syn::Lit::Str(s) => Arg::Lit(s.value()),
            syn::Lit::Int(i) => Arg::Lit(i.base10_digits().to_string()),
            syn::Lit::Bool(b) => Arg::Lit(b.value.to_string()),
            other => Arg::Lit(quote!(#other).to_string()),
        },
        Expr::MethodCall(mc)
            if matches!(
                mc.method.to_string().as_str(),
                "to_string" | "to_owned" | "into"
            ) =>
        {
            if let Expr::Lit(syn::ExprLit {
                lit: syn::Lit::Str(s),
                ..
            }) = unwrap_expr(&mc.receiver)
            {
                Arg::Lit(s.value())
            } else {
                expr_to_arg(&mc.receiver, bindings, inputs)
            }
        }
        Expr::Path(p) if p.path.segments.len() == 1 => {
            let name = p.path.segments[0].ident.to_string();
            if let Some(task) = bindings.get(&name) {
                Arg::Ref(task.clone())
            } else if inputs.contains(&name) {
                Arg::Input(name)
            } else {
                Arg::Lit(name)
            }
        }
        other => Arg::Lit(quote!(#other).to_string().replace(char::is_whitespace, "")),
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

/// Short name for a callee path (its last segment), used to derive a
/// default DAG task name.
fn path_leaf(p: &Path) -> String {
    p.segments
        .last()
        .map(|s| s.ident.to_string())
        .unwrap_or_else(|| "task".to_string())
}

fn analyze_workflow(func: &ItemFn) -> Vec<Node> {
    let mut bindings: std::collections::HashMap<String, String> = Default::default();
    let mut used: std::collections::HashSet<String> = Default::default();
    let inputs: std::collections::HashSet<String> = fn_args(func)
        .iter()
        .map(|(i, _)| i.to_string())
        .collect();
    let mut nodes = Vec::new();

    let mut record = |callee: Path, raw_args: Vec<Expr>, binding: Option<String>,
                       bindings: &std::collections::HashMap<String, String>|
     -> (Node, Option<String>) {
        let base = binding.clone().unwrap_or_else(|| path_leaf(&callee));
        let mut task = kebab(&base);
        let mut n = 1;
        while used.contains(&task) {
            n += 1;
            task = format!("{}-{n}", kebab(&base));
        }
        used.insert(task.clone());
        let args = raw_args
            .iter()
            .map(|a| expr_to_arg(a, bindings, &inputs))
            .collect();
        (
            Node {
                task: task.clone(),
                callee,
                args,
            },
            binding.map(|_| task),
        )
    };

    for stmt in &func.block.stmts {
        match stmt {
            Stmt::Local(local) => {
                let bind = match &local.pat {
                    syn::Pat::Ident(p) => Some(p.ident.to_string()),
                    syn::Pat::Type(t) => match &*t.pat {
                        syn::Pat::Ident(p) => Some(p.ident.to_string()),
                        _ => None,
                    },
                    _ => None,
                };
                if let (Some(init), Some(bind)) = (&local.init, bind)
                    && let Some((callee, raw)) = call_parts(&init.expr)
                {
                    let (node, task) = record(callee, raw, Some(bind.clone()), &bindings);
                    if let Some(t) = task {
                        bindings.insert(bind, t);
                    }
                    nodes.push(node);
                }
            }
            Stmt::Expr(expr, _) => {
                if let Some((callee, raw)) = call_parts(expr) {
                    let (node, _) = record(callee, raw, None, &bindings);
                    nodes.push(node);
                }
            }
            _ => {}
        }
    }
    nodes
}

fn node_tokens(node: &Node) -> TokenStream2 {
    let task = &node.task;
    let callee = &node.callee;

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
        Arg::Ref(dep) => quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                __deps.push(#dep.to_string());
                let mut __v = ::std::string::String::from("{{tasks.");
                __v.push_str(#dep);
                __v.push_str(".outputs.result}}");
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(__v),
                    ..::core::default::Default::default()
                });
            }
        },
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
            __tasks.push(::cargo_athena::api::DagTask {
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
            });
        }
    }
}

#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let name_override = parse_name_override(&attr.into());

    let vis = &func.vis;
    let ident = func.sig.ident.clone();
    let rust_name = ident.to_string();
    let argo_name = make_argo_name(&name_override, &rust_name);

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
    let nodes = analyze_workflow(&func);
    let node_blocks: Vec<_> = nodes.iter().map(node_tokens).collect();

    // Distinct callee paths -> recurse their `collect` (force-links them).
    let mut seen_callees = std::collections::HashSet::new();
    let callee_paths: Vec<&Path> = nodes
        .iter()
        .map(|n| &n.callee)
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

    let expanded = quote! {
        // The body is compiled to Argo; the public identity is a type.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo_name;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Workflow;

            fn build(_ctx: &::cargo_athena::BuildCtx)
                -> ::cargo_athena::api::Template
            {
                let mut __tasks: ::std::vec::Vec<::cargo_athena::api::DagTask> =
                    ::std::vec::Vec::new();
                #( #node_blocks )*
                ::cargo_athena::api::Template {
                    name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                    inputs: #inputs_tokens,
                    dag: ::core::option::Option::Some(::cargo_athena::api::DagTemplate {
                        tasks: __tasks,
                    }),
                    ..::core::default::Default::default()
                }
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
