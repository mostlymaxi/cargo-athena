//! Shared helpers used by every attribute-macro entry point.
//!
//! - Name/ident munging: [`kebab`], [`crate_ns`], [`make_argo_name`].
//! - Fn signature shape: [`fn_args`], [`sig_shim`].
//! - Static body scan: [`BodyScan`], [`scan_body`] — collects every
//!   `host!`/`load_artifact!`/`save_artifact!`/`secret*!` literal and
//!   called ident, for the `#[fragment]` resource closure.
//! - Declaration-macro gating: [`DECL_MACROS`]/[`DeclKind`]/[`decl_kind`],
//!   [`DeclRewrite`], [`with_host_rewritten`] — rewrite public form ↔
//!   private form so unattributed call sites fail to compile.
//! - YAML-1.1 safety: [`yaml_ambiguous`], [`check_yaml_safe_names`].
//! - Token slices: [`str_slice`], [`secret_slice_tokens`].
//! - AST navigation: [`unwrap_expr`], [`is_builder_method`].

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{Expr, ItemFn, Type, visit::Visit, visit_mut::VisitMut};

pub(crate) fn kebab(s: &str) -> String {
    s.replace('_', "-").to_ascii_lowercase()
}

/// A never-run signature shim with the fn's *real* arg + return types,
/// as an inherent fn on the identity struct. Lets the `#[workflow]` ghost
/// type-check data flow (`Callee::__athena_sig(args)`) while the public
/// identity stays a unit struct (the wormhole). `pub` + `#[doc(hidden)]`
/// so it resolves cross-crate/module exactly like the type path.
pub(crate) fn sig_shim(ident: &syn::Ident, func: &ItemFn) -> TokenStream2 {
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
pub(crate) fn fn_args(func: &ItemFn) -> Vec<(syn::Ident, Box<Type>)> {
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
pub(crate) const DECL_MACROS: &[(&str, &str, DeclKind)] = &[
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
    ("secret", "__cargo_athena_secret", DeclKind::Secret),
    (
        "secret_opt",
        "__cargo_athena_secret_opt",
        DeclKind::SecretOpt,
    ),
]; // NB: keep in sync with the macro pairs in cargo-athena-core.

#[derive(Clone, Copy, PartialEq)]
pub(crate) enum DeclKind {
    Host,
    InArtifact,
    OutArtifact,
    /// `secret!(name, key)` — required env-sourced K8s secret. Two
    /// string literals; collected into `BodyScan.secrets` with
    /// `optional = false`.
    Secret,
    /// `secret_opt!(name, key)` — optional variant; `optional = true`.
    SecretOpt,
}

pub(crate) fn decl_kind(mac: &syn::Macro) -> Option<(DeclKind, &'static str)> {
    let last = mac.path.segments.last()?;
    DECL_MACROS
        .iter()
        .find(|(public, ..)| last.ident == public)
        .map(|(_, private, kind)| (*kind, *private))
}

/// First string-literal argument of a decl macro (`host!("p")`,
/// `save_artifact!("n", expr)` → `"n"`). Literal-only by contract.
pub(crate) fn first_str_lit(mac: &syn::Macro) -> Option<String> {
    let args = mac
        .parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    match args.first()? {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// `secret!`/`secret_opt!` take two string literals
/// (`(secret_name, key)`). Returns the pair when the call is
/// well-formed; the public-form gate already errors on bad shape.
pub(crate) fn two_str_lits(mac: &syn::Macro) -> Option<(String, String)> {
    let args = mac
        .parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    let mut it = args.into_iter();
    let lit = |e: Expr| match e {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    };
    Some((lit(it.next()?)?, lit(it.next()?)?))
}

/// Static collector: every decl-macro literal (across all branches) + every
/// called ident, used for the cross-item `#[fragment]` closure.
#[derive(Default)]
pub(crate) struct BodyScan {
    pub(crate) host_paths: Vec<String>,
    pub(crate) in_artifacts: Vec<String>,
    pub(crate) out_artifacts: Vec<String>,
    /// `(secret_name, key, optional)` triples from `secret!` /
    /// `secret_opt!` declarations. Same union/closure semantics as the
    /// other decl buckets — propagated through `#[fragment]`.
    pub(crate) secrets: Vec<(String, String, bool)>,
    pub(crate) callees: Vec<String>,
}

impl<'ast> Visit<'ast> for BodyScan {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some((kind, _)) = decl_kind(mac) {
            match kind {
                DeclKind::Host => {
                    if let Some(n) = first_str_lit(mac) {
                        self.host_paths.push(n);
                    }
                }
                DeclKind::InArtifact => {
                    if let Some(n) = first_str_lit(mac) {
                        self.in_artifacts.push(n);
                    }
                }
                DeclKind::OutArtifact => {
                    if let Some(n) = first_str_lit(mac) {
                        self.out_artifacts.push(n);
                    }
                }
                DeclKind::Secret => {
                    if let Some((n, k)) = two_str_lits(mac) {
                        self.secrets.push((n, k, false));
                    }
                }
                DeclKind::SecretOpt => {
                    if let Some((n, k)) = two_str_lits(mac) {
                        self.secrets.push((n, k, true));
                    }
                }
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

pub(crate) fn scan_body(func: &ItemFn) -> BodyScan {
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
    s.secrets.sort();
    s.secrets.dedup();
    s
}

/// Render a `BodyScan.secrets` list as a `&[(&'static str, &'static
/// str, bool)]` token slice — the shape `FragmentReg.secrets` /
/// `BuildCtx::resolved_secrets` expect.
pub(crate) fn secret_slice_tokens(secrets: &[(String, String, bool)]) -> proc_macro2::TokenStream {
    let entries = secrets.iter().map(|(n, k, opt)| {
        quote! { (#n, #k, #opt) }
    });
    quote! { &[ #( #entries ),* ] }
}

pub(crate) fn str_slice(items: &[String]) -> TokenStream2 {
    let lits = items.iter().map(|s| s.as_str());
    quote! { &[ #( #lits ),* ] }
}

/// Defining crate's name (set by Cargo while this crate compiles), used to
/// namespace Argo template names so they're globally unique across crates.
/// `true` if `ty` is `Artifact<...>` in any of its three written forms
/// (`Artifact<T>`, `cargo_athena::Artifact<T>`, `::cargo_athena::Artifact<T>`).
/// Used by `#[container]` and `#[workflow]` to discriminate parameter-
/// flow from artifact-flow per arg and per return; everything else falls
/// back to parameter-flow so existing templates stay byte-identical.
pub(crate) fn is_artifact_ty(ty: &Type) -> bool {
    let Type::Path(tp) = ty else {
        return false;
    };
    tp.path
        .segments
        .last()
        .is_some_and(|s| s.ident == "Artifact" && !s.arguments.is_empty())
}

pub(crate) fn crate_ns() -> String {
    std::env::var("CARGO_CRATE_NAME")
        .or_else(|_| std::env::var("CARGO_PKG_NAME"))
        .unwrap_or_else(|_| "crate".to_string())
}

/// Final Argo resource name: an explicit `name = "..."` override, else
/// `<crate>-<fn>` (kebab, DNS-1123-ish, globally unique).
pub(crate) fn make_argo_name(name_override: &Option<String>, rust_name: &str) -> String {
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
pub(crate) fn yaml_ambiguous(s: &str) -> Option<&'static str> {
    match s.to_ascii_lowercase().as_str() {
        "y" | "yes" | "n" | "no" | "true" | "false" | "on" | "off" => Some("a YAML 1.1 boolean"),
        "null" | "~" => Some("a YAML 1.1 null"),
        _ => None,
    }
}

/// Reject argument names (→ Argo parameter names) and `name = "…"`
/// overrides that a YAML 1.1 parser would silently mis-type. Spanned at
/// the offending token so the fix is obvious. Synthetic `if`/`else`
/// wrappers reuse the `#[workflow]` codegen path, so their captured
/// inputs are covered by the same check.
pub(crate) fn check_yaml_safe_names(
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

/// Rewrites every decl macro (`host!`, `load_artifact*!`,
/// `save_artifact*!`) the attribute macro can see into its private real
/// form. Enforcement half of the gate: the *public* forms are hard
/// `compile_error!`s, so any invocation we don't rewrite here — a plain
/// fn, a `#[workflow]`, or nested inside another macro's tokens — fails to
/// compile instead of silently doing nothing.
pub(crate) struct DeclRewrite;

impl VisitMut for DeclRewrite {
    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        if let Some((_, private)) = decl_kind(mac) {
            let p: syn::Path = syn::parse_str(&format!("::cargo_athena::{private}")).unwrap();
            mac.path = p;
        }
        syn::visit_mut::visit_macro_mut(self, mac);
    }
}

/// Clone `func`, swap its visible decl macros for the private forms; the
/// original is left intact for the (pre-rewrite) static scan.
pub(crate) fn with_host_rewritten(func: &ItemFn) -> ItemFn {
    let mut out = func.clone();
    DeclRewrite.visit_item_fn_mut(&mut out);
    out
}

/// The builder methods peeled off task calls (kept in sync with
/// `peel_builders`). Stripped from the `#[workflow]` ghost since they
/// aren't real methods on the values.
pub(crate) fn is_builder_method(m: &syn::Ident) -> bool {
    matches!(
        m.to_string().as_str(),
        "continue_on" | "on_exit" | "on_success" | "on_failure" | "on_error" | "hook_if"
    )
}

pub(crate) fn unwrap_expr(e: &Expr) -> &Expr {
    match e {
        Expr::Paren(p) => unwrap_expr(&p.expr),
        Expr::Group(g) => unwrap_expr(&g.expr),
        Expr::Reference(r) => unwrap_expr(&r.expr),
        other => other,
    }
}
