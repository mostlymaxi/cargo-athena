//! The `#[fragment]` proc-macro expansion.
//!
//! A `#[fragment]` is a plain helper fn (not a template) that carries
//! pod-resource declarations (`host!`/`load_artifact!`/`save_artifact!`/
//! `secret!`) across function boundaries into every `#[container]` that
//! transitively calls it. It runs as ordinary Rust inside the caller's
//! pod and cannot be called from a `#[workflow]`.

use proc_macro::TokenStream;
use quote::quote;
use syn::ItemFn;

use crate::utils::{scan_body, secret_slice_tokens, str_slice, with_host_rewritten};

pub(crate) fn expand(_attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    let rust_name = func.sig.ident.to_string();
    let scan = scan_body(&func);
    let func = with_host_rewritten(&func);
    let host_slice = str_slice(&scan.host_paths);
    let in_art_slice = str_slice(&scan.in_artifacts);
    let out_art_slice = str_slice(&scan.out_artifacts);
    let secret_slice = secret_slice_tokens(&scan.secrets);
    let callee_slice = str_slice(&scan.callees);
    // `pvc!(T)` usages — emit as `&[<T as Pvc>::ARGO_NAME, …]` so the
    // string IS the type's actual argo name (resolved at user-build
    // time via the trait const), no proc-macro-side stringification of
    // type paths. Force-links each `T`'s `Pvc` impl crate too.
    let pvc_argo_names_tok = pvc_argo_names_tokens(&scan.pvc_types);

    let expanded = quote! {
        #func

        ::cargo_athena::inventory::submit! {
            ::cargo_athena::FragmentReg {
                rust_name: #rust_name,
                host_paths: #host_slice,
                in_artifacts: #in_art_slice,
                out_artifacts: #out_art_slice,
                secrets: #secret_slice,
                pvc_argo_names: #pvc_argo_names_tok,
                callees: #callee_slice,
            }
        }
    };
    expanded.into()
}

/// `&[<T0 as ::cargo_athena::Pvc>::ARGO_NAME, <T1 as …>::ARGO_NAME, …]`.
fn pvc_argo_names_tokens(types: &[syn::Path]) -> proc_macro2::TokenStream {
    if types.is_empty() {
        return quote! { &[] };
    }
    let entries = types
        .iter()
        .map(|p| quote! { <#p as ::cargo_athena::Pvc>::ARGO_NAME });
    quote! { &[ #( #entries ),* ] }
}
