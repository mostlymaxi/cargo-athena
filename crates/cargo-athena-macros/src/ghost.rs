//! Ghost type-check fn for `#[workflow]` bodies.
//!
//! A `#[workflow]` body isn't executed; it's *analyzed* into a DAG.
//! [`ghost_fn`] emits a hidden, never-called fn that mirrors the body
//! with builder chains reduced to their receiver plus one statement per
//! hook target call, and every `C(args)` rewritten to
//! `C::__athena_sig(args)`, so rustc fully type-checks arg/arity/field/
//! return flow on the analyzed body — a bad `a.field`, wrong type, or
//! consuming a non-returning `#[workflow]` becomes a compile error.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, ItemFn, visit_mut::VisitMut};

use crate::utils::{is_builder_method, unwrap_expr};

/// A hook target `t` / `t(args)` as the call the ghost type-checks — a
/// bare path becomes the zero-arg call `t()` (a template with inputs
/// can't be a bare hook target). Other shapes are `None`: the analyzer
/// rejects them with a targeted error; a ghost duplicate is noise.
fn hook_call(e: &Expr) -> Option<Expr> {
    match unwrap_expr(e) {
        Expr::Call(_) => Some(e.clone()),
        Expr::Path(p) => Some(syn::parse_quote!(#p ())),
        _ => None,
    }
}

/// Rewrites a clone of a `#[workflow]` body into the never-run ghost:
/// reduce builder chains to their receiver — keeping each hook target
/// as its own `let _ = t(args);` statement so hooks are checked exactly
/// like task calls — and rewrite every template call `C(args)` →
/// `C::__athena_sig(args)`. The result is faithful Rust (move semantics
/// intact — fan-out needs an explicit `.clone()`, which mirrors Argo
/// copying the output param into each consumer, and a hook arg is one
/// more consumer), so rustc enforces arg/field/return types on the
/// analyzed body.
struct GhostRewrite;

impl VisitMut for GhostRewrite {
    fn visit_expr_mut(&mut self, e: &mut Expr) {
        let mut hooks: Vec<Expr> = Vec::new();
        let mut fallible = false;
        while let Expr::MethodCall(mc) = e {
            if !is_builder_method(&mc.method) {
                break;
            }
            fallible |= mc.method == "continue_on";
            if mc.method == "hook_if" {
                // `.hook_if("expr" = t, …)` — targets are the assign
                // right sides; the string keys aren't Rust values.
                hooks.extend(mc.args.iter().filter_map(|a| match unwrap_expr(a) {
                    Expr::Assign(asn) => hook_call(&asn.right),
                    _ => None,
                }));
            } else if mc.method != "continue_on" {
                // `.continue_on(failed, error)` args are bare option
                // idents, not calls — nothing to type-check there.
                hooks.extend(mc.args.iter().filter_map(hook_call));
            }
            let recv = (*mc.receiver).clone();
            *e = recv;
        }
        if !hooks.is_empty() {
            // Peeled outside-in; restore source order so move errors
            // point at the later consumer, like straight-line code.
            hooks.reverse();
            let base = e.clone();
            *e = syn::parse_quote!({
                let __athena_ghost_base = #base;
                #( let _ = #hooks; )*
                __athena_ghost_base
            });
        }
        if let Expr::Call(c) = e
            && let Expr::Path(p) = &mut *c.func
        {
            p.path
                .segments
                .push(syn::PathSegment::from(format_ident!("__athena_sig")));
        }
        syn::visit_mut::visit_expr_mut(self, e);
        // After recursion so the wrapper's inner call isn't re-visited.
        if fallible {
            let inner = e.clone();
            *e = syn::parse_quote!(::cargo_athena::__athena_fallible(#inner));
        }
    }
}

/// Build the hidden, never-called type-check ghost for a `#[workflow]`.
pub(crate) fn ghost_fn(func: &ItemFn) -> TokenStream2 {
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
