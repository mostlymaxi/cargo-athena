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

pub(crate) use cargo_athena_api::munge::kebab;

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
    // `pvc!(Type)` — no private declarative form (DeclRewrite rewrites
    // it directly to `Path::new(<Type as Pvc>::MOUNT_PATH)`), but it
    // IS a decl macro the body scan + gating need to recognize.
    ("pvc", "__cargo_athena_pvc_unused", DeclKind::Pvc),
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
    /// `pvc!(Type)` — mount a PVC declared via `#[ephemeral_pvc]` /
    /// `#[external_pvc]`. Single type-path argument.
    Pvc,
}

pub(crate) fn decl_kind(mac: &syn::Macro) -> Option<(DeclKind, &'static str)> {
    let last = mac.path.segments.last()?;
    DECL_MACROS
        .iter()
        .find(|(public, ..)| last.ident == public)
        .map(|(_, private, kind)| (*kind, *private))
}

/// Public macro name (`"host"`, `"load_artifact_str"`, `"secret"`, …)
/// if `mac` is one of the recognized decl macros — used by
/// `DeclRewrite` to dispatch the per-macro bake at expansion time.
pub(crate) fn decl_public_name(mac: &syn::Macro) -> Option<&'static str> {
    let last = mac.path.segments.last()?;
    DECL_MACROS
        .iter()
        .find(|(public, ..)| last.ident == public)
        .map(|(public, ..)| *public)
}

/// First string-literal argument of a decl macro, ignoring the rest
/// (`save_artifact!("n", expr)` → `"n"`). For the two-arg `save_*`
/// forms the BodyScan only needs the name — arity is enforced by
/// [`save_args`] on the rewrite side (a bad shape leaves the macro
/// unrewritten so the public `compile_error!` gate fires).
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

/// The SOLE string-literal argument of a single-arg decl macro
/// (`host!("p")`, `load_artifact!("n")`). Exact arity: surplus args
/// make this return `None`, so the invocation is left unrewritten
/// (and unscanned) and the public form's `compile_error!` gate fires
/// with its usage message instead of the extras being silently
/// dropped.
pub(crate) fn only_str_lit(mac: &syn::Macro) -> Option<String> {
    let args = mac
        .parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    if args.len() != 1 {
        return None;
    }
    match args.first()? {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Some(s.value()),
        _ => None,
    }
}

/// `secret!`/`secret_opt!` take exactly two string literals
/// (`(secret_name, key)`). Returns the pair when the call is
/// well-formed; anything else (including surplus args) returns `None`
/// so the public-form `compile_error!` gate fires.
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
    let pair = (lit(it.next()?)?, lit(it.next()?)?);
    if it.next().is_some() {
        return None;
    }
    Some(pair)
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
    /// Type paths referenced by `pvc!(Type)` calls. Stored as
    /// `syn::Path` so the proc macro can emit
    /// `<Type as Pvc>::ARGO_NAME` for cross-crate force-linking
    /// (and so `Type` can be a path like `crate::pvcs::BuildCache`).
    pub(crate) pvc_types: Vec<syn::Path>,
    pub(crate) callees: Vec<String>,
}

impl<'ast> Visit<'ast> for BodyScan {
    fn visit_macro(&mut self, mac: &'ast syn::Macro) {
        if let Some((kind, _)) = decl_kind(mac) {
            match kind {
                DeclKind::Host => {
                    if let Some(n) = only_str_lit(mac) {
                        self.host_paths.push(n);
                    }
                }
                DeclKind::InArtifact => {
                    if let Some(n) = only_str_lit(mac) {
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
                DeclKind::Pvc => {
                    if let Ok(path) = syn::parse2::<syn::Path>(mac.tokens.clone()) {
                        self.pvc_types.push(path);
                    }
                }
            }
        }
        syn::visit::visit_macro(self, mac);
    }

    fn visit_expr_call(&mut self, call: &'ast syn::ExprCall) {
        // Record the callee's *function name* (the last path segment) as a
        // candidate `#[fragment]` callee. A fragment is keyed by its own
        // ident in `FragmentReg`, so the last segment is what matches: this
        // catches a bare `frag()` AND a qualified `path::to::frag()` (the
        // old `len() == 1` form silently dropped the latter, losing its
        // `host!` / `secret!` / artifact propagation).
        //
        // Matching is by bare name because a proc macro can't resolve
        // paths/types. Two consequences, both inherent (not introduced by
        // accepting qualified paths) and accepted by design:
        //   - a non-fragment call sharing a fragment's name over-propagates
        //     (harmless extra mounts; the `FragmentReg` map is keyed by
        //     ident, so same-named fragments already collide regardless);
        //   - an aliased call (`use frag as f; f()`) records the alias,
        //     which won't match the fragment's real ident, so it stays
        //     unpropagated. Closures / fn-pointers / method calls aren't
        //     path-calls, so they're never recorded (no false match).
        if let Expr::Path(p) = &*call.func
            && let Some(last) = p.path.segments.last()
        {
            self.callees.push(last.ident.to_string());
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
    // Dedup pvc_types by their stringified form (`syn::Path` doesn't
    // impl `Ord` directly, so do it via the token-string key).
    let mut seen = std::collections::HashSet::new();
    s.pvc_types
        .retain(|p| seen.insert(quote::ToTokens::to_token_stream(p).to_string()));
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

/// Defining crate's name (set by Cargo while this crate compiles), used to
/// namespace Argo template names so they're globally unique across crates.
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

/// Why `name` is not a valid RFC-1123 subdomain, or `None` if it is.
/// The single validator behind every `name = "..."` override
/// (`#[container]` / `#[workflow]` / `#[ephemeral_pvc]` /
/// `#[external_pvc]`) — the string becomes a k8s `metadata.name`, and
/// k8s rejects anything else at admission, so fail at compile time.
pub(crate) fn dns1123_violation(name: &str) -> Option<String> {
    if name.is_empty() {
        return Some("it is empty".to_string());
    }
    if name.len() > 253 {
        return Some(format!("it is {} chars (max 253)", name.len()));
    }
    for label in name.split('.') {
        if label.is_empty() {
            return Some(
                "it has an empty dot-separated label (leading, trailing, or doubled `.`)"
                    .to_string(),
            );
        }
        if label.len() > 63 {
            return Some(format!("label `{label}` is {} chars (max 63)", label.len()));
        }
        if let Some(c) = label
            .chars()
            .find(|c| !(c.is_ascii_lowercase() || c.is_ascii_digit() || *c == '-'))
        {
            return Some(format!(
                "it contains `{c}` (allowed: lowercase alphanumeric, `-`, `.`)"
            ));
        }
        if label.starts_with('-') || label.ends_with('-') {
            return Some(format!("label `{label}` starts or ends with `-`"));
        }
    }
    None
}

/// Validate a `name = "..."` override as a DNS-1123 subdomain, spanned
/// at the literal.
pub(crate) fn check_dns1123_name(lit: &syn::LitStr) -> syn::Result<()> {
    let v = lit.value();
    match dns1123_violation(&v) {
        None => Ok(()),
        Some(why) => Err(syn::Error::new_spanned(
            lit,
            format!(
                "`name = \"{v}\"` is not a valid DNS-1123 subdomain ({why}); \
                 k8s would reject the emitted resource at admission"
            ),
        )),
    }
}

/// Why `name` is not a valid k8s env-var name
/// (`[-._a-zA-Z][-._a-zA-Z0-9]*`), or `None` if it is. Anything else
/// fails at pod admission.
pub(crate) fn env_var_name_violation(name: &str) -> Option<String> {
    let ok_head = |c: char| c.is_ascii_alphabetic() || matches!(c, '-' | '.' | '_');
    let ok_tail = |c: char| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_');
    let mut chars = name.chars();
    let Some(head) = chars.next() else {
        return Some("it is empty".to_string());
    };
    if !ok_head(head) {
        return Some(format!(
            "it starts with `{head}` (must be a letter, `-`, `.`, or `_`)"
        ));
    }
    if let Some(c) = chars.find(|c| !ok_tail(*c)) {
        return Some(format!(
            "it contains `{c}` (allowed: alphanumeric, `-`, `.`, `_`)"
        ));
    }
    None
}

/// Why `key` is not a valid k8s annotation key (a qualified name:
/// optional DNS-1123 subdomain prefix + `/` + a 1..=63-char name that
/// starts/ends alphanumeric with `-`/`_`/`.` allowed inside), or
/// `None` if it is.
pub(crate) fn annotation_key_violation(key: &str) -> Option<String> {
    let (prefix, name) = match key.rsplit_once('/') {
        Some((p, n)) => (Some(p), n),
        None => (None, key),
    };
    if let Some(p) = prefix {
        if p.is_empty() {
            return Some("its `/`-prefix is empty".to_string());
        }
        // A prefix must be a DNS-1123 subdomain, but is conventionally
        // written lowercase anyway — reuse the subdomain validator.
        if let Some(why) = dns1123_violation(p) {
            return Some(format!(
                "its prefix `{p}` is not a DNS-1123 subdomain ({why})"
            ));
        }
    }
    if name.is_empty() {
        return Some("its name part is empty".to_string());
    }
    if name.len() > 63 {
        return Some(format!("its name part is {} chars (max 63)", name.len()));
    }
    let alnum = |c: char| c.is_ascii_alphanumeric();
    if !name.starts_with(alnum) || !name.ends_with(alnum) {
        return Some("its name part must start and end alphanumeric".to_string());
    }
    if let Some(c) = name
        .chars()
        .find(|c| !(c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.')))
    {
        return Some(format!(
            "it contains `{c}` (allowed: alphanumeric, `-`, `_`, `.`)"
        ));
    }
    None
}

/// Reject fn shapes the macros cannot lower, with the error spanned at
/// the offending piece instead of surfacing as a rustc error inside a
/// hidden generated item. Generics are structurally unsupported (the
/// unit-struct identity + `INPUTS` const need ONE concrete signature);
/// non-identifier argument patterns have no Argo parameter name.
pub(crate) fn check_fn_shape(func: &ItemFn, kind: &str) -> Result<(), TokenStream> {
    let g = &func.sig.generics;
    if !g.params.is_empty() || g.where_clause.is_some() {
        let target: TokenStream2 = if g.params.is_empty() {
            quote::ToTokens::to_token_stream(&g.where_clause)
        } else {
            quote::ToTokens::to_token_stream(g)
        };
        return Err(syn::Error::new_spanned(
            target,
            format!(
                "#[{kind}] does not support generic functions: the template \
                 lowers to one unit-struct identity with one concrete \
                 signature (its inputs become a fixed Argo parameter list)"
            ),
        )
        .to_compile_error()
        .into());
    }
    for arg in &func.sig.inputs {
        match arg {
            syn::FnArg::Receiver(r) => {
                return Err(syn::Error::new_spanned(
                    r,
                    format!("#[{kind}] must be a free function (no `self`)"),
                )
                .to_compile_error()
                .into());
            }
            syn::FnArg::Typed(pt) => {
                if !matches!(&*pt.pat, syn::Pat::Ident(_)) {
                    return Err(syn::Error::new_spanned(
                        &pt.pat,
                        format!(
                            "unsupported parameter pattern in a #[{kind}]: \
                             bind a plain identifier (each parameter becomes \
                             a named Argo input)"
                        ),
                    )
                    .to_compile_error()
                    .into());
                }
            }
        }
    }
    Ok(())
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
    name_override: &Option<syn::LitStr>,
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
    if let Some(lit) = name_override
        && let Some(why) = yaml_ambiguous(&lit.value())
    {
        let n = lit.value();
        return Err(syn::Error::new_spanned(
            lit,
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

// Name/path/env-var derivers live in `cargo_athena_api::munge` —
// proc-macro side (here) AND emit side (cargo-athena-core) both call
// into that single source so they cannot drift silently. Prior
// versions of athena mirrored the FNV-1a hash + the env-var munge
// formula in each crate and pinned them with algorithm tests; that
// drift risk is gone now.
pub(crate) use cargo_athena_api::munge::{
    host_mount_path, in_artifact_path, out_artifact_path, pvc_mount_path, secret_env_name,
};

/// Rewrites every decl macro the attribute macro can see into its
/// final form. Enforcement half of the gate: the *public* forms are
/// hard `compile_error!`s, so any invocation we don't rewrite here — a
/// plain fn, a `#[workflow]`, or nested inside another macro's tokens
/// — fails to compile instead of silently doing nothing.
///
/// Every decl macro precomputes its derived strings (mount path, env
/// var name, artifact file path) at proc-macro expansion time and
/// emits an expression carrying those literals. Runtime helpers
/// (`rt::load_artifact`, `rt::secret_value`, …) take the pre-baked
/// strings; they no longer rebuild them on every call.
///
/// - `host!("/p")` → `Path::new("/athena/mounts/<hash>")`
/// - `load_artifact!("k")` → `rt::load_artifact("/athena/artifacts/in/k", "k")`
/// - `load_artifact_str!("k")` → `rt::load_artifact_str("/athena/artifacts/in/k", "k")`
/// - `save_artifact!("k", data)` → `rt::save_artifact("/athena/artifacts/out/k", "k", data)`
/// - `save_artifact_str!("k", data)` → `rt::save_artifact_str("/athena/artifacts/out/k", "k", data)`
/// - `secret!("s", "k")` → `rt::secret_value("ATHENA_SEC_S__K", "s", "k")`
/// - `secret_opt!("s", "k")` → `rt::secret_value_opt("ATHENA_SEC_S__K")`
pub(crate) struct DeclRewrite;

impl VisitMut for DeclRewrite {
    fn visit_expr_mut(&mut self, expr: &mut Expr) {
        if let Expr::Macro(em) = expr
            && let Some(public) = decl_public_name(&em.mac)
            && let Some(new_expr) = rewrite_decl_call(public, &em.mac)
        {
            *expr = new_expr;
        }
        syn::visit_mut::visit_expr_mut(self, expr);
    }

    fn visit_stmt_mut(&mut self, stmt: &mut syn::Stmt) {
        // `save_artifact!(...);` at statement position parses as
        // `Stmt::Macro`, which `visit_expr_mut` never sees. Rewrite
        // those by hand into `Stmt::Expr(new_expr, semi)`.
        if let syn::Stmt::Macro(sm) = stmt
            && let Some(public) = decl_public_name(&sm.mac)
            && let Some(new_expr) = rewrite_decl_call(public, &sm.mac)
        {
            *stmt = syn::Stmt::Expr(new_expr, sm.semi_token);
        }
        syn::visit_mut::visit_stmt_mut(self, stmt);
    }
}

/// Bake-time replacement for a recognized decl-macro invocation.
/// Returns `None` if the arg shape doesn't match (literal-only by
/// contract); the original macro is left intact so the public form's
/// `compile_error!` gate fires with the correct diagnostic.
fn rewrite_decl_call(public: &str, mac: &syn::Macro) -> Option<Expr> {
    match public {
        "host" => {
            let lit = only_str_lit(mac)?;
            let mount = host_mount_path(&lit);
            Some(syn::parse_quote! {
                ::std::path::Path::new(#mount)
            })
        }
        "load_artifact" => {
            let name = only_str_lit(mac)?;
            let path = in_artifact_path(&name);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::load_artifact(#path, #name)
            })
        }
        "load_artifact_str" => {
            let name = only_str_lit(mac)?;
            let path = in_artifact_path(&name);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::load_artifact_str(#path, #name)
            })
        }
        "save_artifact" => {
            let (name, data) = save_args(mac)?;
            let path = out_artifact_path(&name);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::save_artifact(#path, #name, #data)
            })
        }
        "save_artifact_str" => {
            let (name, data) = save_args(mac)?;
            let path = out_artifact_path(&name);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::save_artifact_str(#path, #name, #data)
            })
        }
        "secret" => {
            let (name, key) = two_str_lits(mac)?;
            let env = secret_env_name(&name, &key);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::secret_value(#env, #name, #key)
            })
        }
        "secret_opt" => {
            let (name, key) = two_str_lits(mac)?;
            let env = secret_env_name(&name, &key);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::secret_value_opt(#env)
            })
        }
        "pvc" => {
            // `pvc!(Type)` — the user's literal is a type-path; emit
            // `Path::new(<Type as Pvc>::MOUNT_PATH)` so the const is
            // resolved at the user's build time (the Pvc impl was
            // generated by `#[ephemeral_pvc]` / `#[external_pvc]` with
            // a pre-baked mount-path string).
            let path = syn::parse2::<syn::Path>(mac.tokens.clone()).ok()?;
            Some(syn::parse_quote! {
                ::std::path::Path::new(<#path as ::cargo_athena::Pvc>::MOUNT_PATH)
            })
        }
        _ => None,
    }
}

/// Parse `save_artifact!("name", data_expr)` into the two pieces.
fn save_args(mac: &syn::Macro) -> Option<(String, Expr)> {
    let args = mac
        .parse_body_with(syn::punctuated::Punctuated::<Expr, syn::Token![,]>::parse_terminated)
        .ok()?;
    let mut it = args.into_iter();
    let name_expr = it.next()?;
    let data = it.next()?;
    if it.next().is_some() {
        return None;
    }
    let name = match name_expr {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => s.value(),
        _ => return None,
    };
    Some((name, data))
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

// Algorithm tests for the munge helpers live in
// `cargo_athena_api::munge` (their single source of truth). No
// proc-macro-side pin tests needed: drift is impossible by
// construction now that both sides import the same fns.

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dns1123_subdomain_shapes() {
        for ok in [
            "a",
            "abc",
            "a-b",
            "a.b",
            "a-1.b-2",
            "0a",
            "x".repeat(63).as_str(),
        ] {
            assert!(dns1123_violation(ok).is_none(), "{ok:?} should be valid");
        }
        // `.foo` / `a..b` / `foo.` were accepted by the old bespoke PVC
        // check (it never looked at per-label emptiness) — pinned here.
        for bad in [
            "", ".foo", "a..b", "foo.", "-a", "a-", "a.-b", "A", "a_b", "a b",
        ] {
            assert!(
                dns1123_violation(bad).is_some(),
                "{bad:?} should be rejected"
            );
        }
        assert!(
            dns1123_violation(&"a".repeat(254)).is_some(),
            "253-char cap"
        );
        let long_label = "a".repeat(64);
        assert!(
            dns1123_violation(&long_label).is_some(),
            "63-char label cap"
        );
    }

    #[test]
    fn env_and_annotation_key_shapes() {
        assert!(env_var_name_violation("PATH").is_none());
        assert!(env_var_name_violation("_x.y-z").is_none());
        assert!(env_var_name_violation("9BAD").is_some());
        assert!(env_var_name_violation("BAD KEY").is_some());
        assert!(env_var_name_violation("").is_some());

        assert!(annotation_key_violation("simple").is_none());
        assert!(annotation_key_violation("example.com/role").is_none());
        assert!(annotation_key_violation("cargo.athena/tag").is_none());
        assert!(annotation_key_violation("bad!key").is_some());
        assert!(annotation_key_violation("/name").is_some());
        assert!(annotation_key_violation("UPPER.Prefix/x").is_some());
        assert!(annotation_key_violation("-edge").is_some());
        assert!(annotation_key_violation(&"a".repeat(64)).is_some());
    }

    #[test]
    fn scan_records_fragment_callees_qualified_or_bare() {
        // A `#[fragment]` is keyed by its bare ident, so a call must be
        // recorded under the fragment's function name regardless of path
        // qualification. Regression: qualified calls used to be dropped
        // (the `len() == 1` gate), losing the fragment's host! / secret! /
        // artifact propagation onto the calling container.
        let f: syn::ItemFn = syn::parse_quote! {
            fn c() {
                bare();
                crate::frags::qualified();
                a::b::c::deep();
                obj.method();
            }
        };
        let s = scan_body(&f);
        assert!(s.callees.contains(&"bare".to_string()));
        assert!(
            s.callees.contains(&"qualified".to_string()),
            "qualified callee dropped"
        );
        assert!(
            s.callees.contains(&"deep".to_string()),
            "deep-qualified callee dropped"
        );
        // A method call is not a path-call, so it is never recorded
        // (fragments are free functions; this avoids a false match).
        assert!(!s.callees.contains(&"method".to_string()));
    }
}
