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

/// Precomputed pod env var name for `secret!(name, key)` /
/// `secret_opt!(name, key)`. Mirror of
/// `cargo_athena_core::secret_env_name`; the pin test asserts both
/// sides agree on the same `(name, key)` → env-var transform so the
/// proc-macro (run-side reader) and emit-side (Argo `secretKeyRef`
/// declaration) never diverge.
fn secret_env_name(name: &str, key: &str) -> String {
    let mut s = String::from("ATHENA_SEC_");
    push_munged(&mut s, name);
    s.push_str("__");
    push_munged(&mut s, key);
    s
}

fn push_munged(out: &mut String, input: &str) {
    for c in input.chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        });
    }
}

/// Precomputed full path for `load_artifact!`/`load_artifact_str!`
/// (input port). Mirror of `cargo_athena_core::rt::IN_DIR` joined with
/// the user-provided name literal.
fn in_artifact_path(name: &str) -> String {
    format!("/athena/artifacts/in/{name}")
}

/// Precomputed full path for `save_artifact!`/`save_artifact_str!`.
fn out_artifact_path(name: &str) -> String {
    format!("/athena/artifacts/out/{name}")
}

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
            let lit = first_str_lit(mac)?;
            let mount = host_mount_path(&lit);
            Some(syn::parse_quote! {
                ::std::path::Path::new(#mount)
            })
        }
        "load_artifact" => {
            let name = first_str_lit(mac)?;
            let path = in_artifact_path(&name);
            Some(syn::parse_quote! {
                ::cargo_athena::rt::load_artifact(#path, #name)
            })
        }
        "load_artifact_str" => {
            let name = first_str_lit(mac)?;
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

    #[test]
    fn secret_env_name_matches_core() {
        // Mirror of core's `secret_env_name`. Emit-side declares the
        // matching Argo `secretKeyRef` with this exact env var; if
        // the two formulas diverge, the run-side reads a missing var
        // and every secret! call panics. Pin to detect drift at
        // build time, not in a user's cluster.
        assert_eq!(
            secret_env_name("github-creds", "token"),
            "ATHENA_SEC_GITHUB_CREDS__TOKEN"
        );
        // Non-alphanumeric chars flatten to `_` (uppercase too).
        assert_eq!(
            secret_env_name("my.secret-name", "api.key"),
            "ATHENA_SEC_MY_SECRET_NAME__API_KEY"
        );
    }
}
