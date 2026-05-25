//! Ghost type-check fn for `#[workflow]` bodies.
//!
//! A `#[workflow]` body isn't executed; it's *analyzed* into a DAG.
//! [`ghost_fn`] emits a hidden, never-called fn that mirrors the body
//! with builder chains stripped and every `C(args)` rewritten to
//! `C::__athena_sig(args)`, so rustc fully type-checks arg/arity/field/
//! return flow on the analyzed body — a bad `a.field`, wrong type, or
//! consuming a non-returning `#[workflow]` becomes a compile error.

use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, ItemFn, visit_mut::VisitMut};

use crate::utils::is_builder_method;

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
