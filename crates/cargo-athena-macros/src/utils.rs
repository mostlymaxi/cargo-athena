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

/// Parallel to [`fn_args`]: for each non-receiver arg, the literal
/// `#[inject("<expr>")]` Argo expression if the arg carries the attr,
/// else `None`. Used by `#[container]` to splice an extra positional
/// argv slot whose value is filled by Argo (rather than declared as an
/// `inputs.parameters` entry). Per-arg attrs the macro doesn't
/// recognize are left intact for rustc to validate or reject.
pub(crate) fn fn_arg_injects(func: &ItemFn) -> Vec<Option<String>> {
    func.sig
        .inputs
        .iter()
        .filter_map(|a| match a {
            syn::FnArg::Typed(pt) => Some(inject_attr_value(&pt.attrs)),
            syn::FnArg::Receiver(_) => None,
        })
        .collect()
}

/// Pull the literal string out of an `#[inject("...")]` attribute. The
/// attribute is `#[inject(<string-literal>)]`; if the body parses as a
/// single `LitStr`, return its value. Anything else is silently
/// ignored here (the macro will surface a clearer error downstream if
/// the user wrote a malformed inject attr).
fn inject_attr_value(attrs: &[syn::Attribute]) -> Option<String> {
    attrs.iter().find_map(|attr| {
        if !attr.path().is_ident("inject") {
            return None;
        }
        attr.parse_args::<syn::LitStr>().ok().map(|s| s.value())
    })
}

/// Strip `#[inject(...)]` attrs from each fn arg so the re-emitted
/// fn (`#func` in the macro output) compiles cleanly under rustc.
pub(crate) fn strip_inject_attrs(func: &mut ItemFn) {
    for a in func.sig.inputs.iter_mut() {
        if let syn::FnArg::Typed(pt) = a {
            pt.attrs.retain(|attr| !attr.path().is_ident("inject"));
        }
    }
}

/// Clone `func` with every `#[inject(...)]`-attributed arg removed
/// entirely (not just the attr stripped). The result is the
/// **caller-visible** signature for `sig_shim` and `ghost_fn` — workflow
/// bodies call this fn without passing inject args, which are filled
/// from Argo at run time.
pub(crate) fn func_without_inject_args(func: &ItemFn) -> ItemFn {
    let mut out = func.clone();
    out.sig.inputs = out
        .sig
        .inputs
        .into_iter()
        .filter(|a| match a {
            syn::FnArg::Typed(pt) => inject_attr_value(&pt.attrs).is_none(),
            syn::FnArg::Receiver(_) => true,
        })
        .collect();
    out
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

/// FNV-1a 64-bit hash of a `host!` literal, emitted as 16 lowercase
/// hex chars. Mirror of `cargo_athena_core::munge_host_path`; the
/// algorithm is pinned by `munge_known_value_pins_algorithm` in core's
/// tests, so the two sides cannot drift silently. Fixed initial state
/// (no `DefaultHasher` random seed): the proc macro hashes at user
/// build time, `cargo athena emit` hashes at emit time in a different
/// process — they MUST agree.
fn munge_host_path(path: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in path.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

/// Precomputed in-pod mount path for `host!("/p")`. Identical to
/// `cargo_athena_core::host_mount_path` by construction; pinned by the
/// algorithm test.
fn host_mount_path(p: &str) -> String {
    format!("/athena/mounts/{}", munge_host_path(p))
}

/// Rewrites every decl macro the attribute macro can see into its
/// final form. Enforcement half of the gate: the *public* forms are
/// hard `compile_error!`s, so any invocation we don't rewrite here — a
/// plain fn, a `#[workflow]`, or nested inside another macro's tokens
/// — fails to compile instead of silently doing nothing.
///
/// `host!` is special: instead of routing to a private declarative
/// macro, the rewrite **replaces the whole expression** with a literal
/// `::std::path::Path::new("/athena/mounts/<precomputed-hash>")`. The
/// mount path is computed once at expansion time (proc macros run at
/// user-build time), so `host!` returns `&'static Path` with zero
/// runtime work — no FNV at startup, no `OnceLock` allocation, no
/// `format!()` on every call.
///
/// The other decl macros (`load_artifact*!`/`save_artifact*!`/
/// `secret*!`) genuinely need a runtime call (reading files, env vars,
/// etc.), so they keep the "swap to private form" path.
pub(crate) struct DeclRewrite;

impl VisitMut for DeclRewrite {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        // host! at expression level → bake in the precomputed mount
        // path BEFORE recursing into children (else the macro_mut pass
        // would still see it as a generic decl macro).
        if let Expr::Macro(em) = expr
            && let Some((DeclKind::Host, _)) = decl_kind(&em.mac)
            && let Some(p) = first_str_lit(&em.mac)
        {
            let mount = host_mount_path(&p);
            *expr = syn::parse_quote! {
                ::std::path::Path::new(#mount)
            };
            // Recurse into the replacement (no nested macros there;
            // a no-op).
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_macro_mut(&mut self, mac: &mut syn::Macro) {
        // host! is replaced at the Expr level above; leave it
        // untouched here so we don't double-process.
        if let Some((kind, private)) = decl_kind(mac)
            && kind != DeclKind::Host
        {
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

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn munge_algorithm_matches_core() {
        // Pinned to the SAME input/output as core's
        // `munge_known_value_pins_algorithm` so the two FNV
        // implementations cannot drift silently. If you change one
        // side, change both - the in-cluster Volume name and the
        // in-pod mount path are both keyed on this hash, so drift
        // breaks every existing deployment.
        assert_eq!(munge_host_path("/var/lib"), "5b8d11771a6f946b");
    }
}
