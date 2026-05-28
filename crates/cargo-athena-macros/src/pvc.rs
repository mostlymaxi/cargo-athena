//! `#[ephemeral_pvc]` and `#[external_pvc]` attribute macros.
//!
//! Both attach to a `pub struct Name;` (unit struct, no fields, no
//! generics) and generate:
//!
//!   * an `impl ::cargo_athena::Pvc for Name { … }` with the right
//!     consts for the chosen lifecycle (size/access_modes/storage
//!     class for ephemeral; claim_name/read_only for external),
//!   * an `inventory::submit!{ PvcReg { … } }` so the emit-side
//!     can materialize the full PVC spec from just the argo name
//!     (which is what fragment closures propagate).
//!
//! Both compute `MOUNT_PATH` as `/athena/pvcs/<fnv-hash-of-argo-name>`
//! at expansion time (zero runtime work, `&'static Path` at the
//! `pvc!(Type)` call site).

use proc_macro::TokenStream;
use quote::quote;
use syn::ItemStruct;

use crate::utils::{kebab, pvc_mount_path};

#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
pub(crate) struct EphemeralArgs {
    /// Override the auto `<crate>-<type-kebab>` Argo name (e.g. when
    /// two crates want to share the same dynamically-created PVC).
    pub(crate) name: Option<String>,
    /// Storage request — required. K8s quantity string, e.g. `"10Gi"`.
    pub(crate) size: Option<String>,
    /// K8s `accessModes` — required, one of:
    /// `"ReadWriteOnce"`/`"ReadWriteMany"`/`"ReadOnlyMany"`/
    /// `"ReadWriteOncePod"`. Required because RWO vs RWX is a real
    /// footgun and athena should never default it silently.
    pub(crate) access_modes: Vec<String>,
    /// `""` (or omit) → use the cluster's default `StorageClass`.
    pub(crate) storage_class: Option<String>,
}

#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
pub(crate) struct ExternalArgs {
    /// Override the auto `<crate>-<type-kebab>` Argo name. Rarely
    /// needed for external PVCs (the `claim_name` is the real
    /// identifier); kept for symmetry with ephemeral.
    pub(crate) name: Option<String>,
    /// Name of the pre-existing PVC in the workflow's namespace.
    /// Required.
    pub(crate) claim_name: Option<String>,
    /// Mount the PVC read-only on every consumer. Defaults to false.
    pub(crate) read_only: bool,
}

pub(crate) fn expand_ephemeral(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = syn::parse_macro_input!(item as ItemStruct);
    if let Err(e) = validate_unit_struct(&item_struct) {
        return e.to_compile_error().into();
    }
    let cfg: EphemeralArgs = match parse_attr(attr) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let span = item_struct.ident.span();
    let size = match cfg.size {
        Some(s) if !s.is_empty() => s,
        _ => {
            return syn::Error::new(
                span,
                "`#[ephemeral_pvc(...)]` requires `size = \"<quantity>\"` \
                 (e.g. `size = \"10Gi\"`)",
            )
            .to_compile_error()
            .into();
        }
    };
    if cfg.access_modes.is_empty() {
        return syn::Error::new(
            span,
            "`#[ephemeral_pvc(...)]` requires `access_modes = [\"...\"]` \
             (e.g. `[\"ReadWriteMany\"]`)",
        )
        .to_compile_error()
        .into();
    }
    for m in &cfg.access_modes {
        if !matches!(
            m.as_str(),
            "ReadWriteOnce" | "ReadWriteMany" | "ReadOnlyMany" | "ReadWriteOncePod"
        ) {
            return syn::Error::new(
                span,
                format!(
                    "`access_modes` entry {m:?} not a K8s access mode \
                     (one of: ReadWriteOnce, ReadWriteMany, ReadOnlyMany, \
                     ReadWriteOncePod)"
                ),
            )
            .to_compile_error()
            .into();
        }
    }
    let storage_class = cfg.storage_class.unwrap_or_default();
    let argo_name = match argo_name_for(&cfg.name, &item_struct.ident) {
        Ok(n) => n,
        Err(e) => return e.to_compile_error().into(),
    };
    let mount_path = pvc_mount_path(&argo_name);
    let access_modes_lit = cfg.access_modes.iter().map(|s| quote!(#s));
    let access_modes_lit_for_reg = cfg.access_modes.iter().map(|s| quote!(#s));

    let name_ident = &item_struct.ident;
    let expanded = quote! {
        #item_struct

        impl ::cargo_athena::Pvc for #name_ident {
            const ARGO_NAME: &'static str = #argo_name;
            const LIFECYCLE: ::cargo_athena::PvcLifecycle =
                ::cargo_athena::PvcLifecycle::Ephemeral;
            const MOUNT_PATH: &'static str = #mount_path;
            const SIZE: &'static str = #size;
            const ACCESS_MODES: &'static [&'static str] = &[ #( #access_modes_lit ),* ];
            const STORAGE_CLASS_NAME: &'static str = #storage_class;
        }

        ::cargo_athena::inventory::submit! {
            ::cargo_athena::PvcReg {
                argo_name: #argo_name,
                mount_path: #mount_path,
                lifecycle: ::cargo_athena::PvcLifecycle::Ephemeral,
                size: #size,
                access_modes: &[ #( #access_modes_lit_for_reg ),* ],
                storage_class_name: #storage_class,
                claim_name: "",
                read_only: false,
            }
        }
    };
    expanded.into()
}

pub(crate) fn expand_external(attr: TokenStream, item: TokenStream) -> TokenStream {
    let item_struct = syn::parse_macro_input!(item as ItemStruct);
    if let Err(e) = validate_unit_struct(&item_struct) {
        return e.to_compile_error().into();
    }
    let cfg: ExternalArgs = match parse_attr(attr) {
        Ok(c) => c,
        Err(e) => return e,
    };
    let span = item_struct.ident.span();
    let claim_name = match cfg.claim_name {
        Some(s) if !s.is_empty() => s,
        _ => {
            return syn::Error::new(
                span,
                "`#[external_pvc(...)]` requires `claim_name = \"<existing-pvc-name>\"`",
            )
            .to_compile_error()
            .into();
        }
    };
    let read_only = cfg.read_only;
    let argo_name = match argo_name_for(&cfg.name, &item_struct.ident) {
        Ok(n) => n,
        Err(e) => return e.to_compile_error().into(),
    };
    let mount_path = pvc_mount_path(&argo_name);

    let name_ident = &item_struct.ident;
    let expanded = quote! {
        #item_struct

        impl ::cargo_athena::Pvc for #name_ident {
            const ARGO_NAME: &'static str = #argo_name;
            const LIFECYCLE: ::cargo_athena::PvcLifecycle =
                ::cargo_athena::PvcLifecycle::External;
            const MOUNT_PATH: &'static str = #mount_path;
            const CLAIM_NAME: &'static str = #claim_name;
            const READ_ONLY: bool = #read_only;
        }

        ::cargo_athena::inventory::submit! {
            ::cargo_athena::PvcReg {
                argo_name: #argo_name,
                mount_path: #mount_path,
                lifecycle: ::cargo_athena::PvcLifecycle::External,
                size: "",
                access_modes: &[],
                storage_class_name: "",
                claim_name: #claim_name,
                read_only: #read_only,
            }
        }
    };
    expanded.into()
}

fn validate_unit_struct(s: &ItemStruct) -> syn::Result<()> {
    if !s.generics.params.is_empty() || s.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            s.ident.span(),
            "`#[ephemeral_pvc]` / `#[external_pvc]` requires a unit struct \
             with no generics: write `pub struct MyPvc;`",
        ));
    }
    match &s.fields {
        syn::Fields::Unit => Ok(()),
        _ => Err(syn::Error::new(
            s.ident.span(),
            "`#[ephemeral_pvc]` / `#[external_pvc]` requires a unit struct \
             (no fields): write `pub struct MyPvc;`",
        )),
    }
}

fn argo_name_for(name_override: &Option<String>, ident: &syn::Ident) -> syn::Result<String> {
    if let Some(n) = name_override {
        // Validate DNS-1123 subdomain shape (k8s PVC name limit).
        if n.is_empty()
            || n.len() > 253
            || !n
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
            || n.starts_with('-')
            || n.ends_with('-')
        {
            return Err(syn::Error::new(
                ident.span(),
                "`name` must be a valid DNS-1123 subdomain: lowercase \
                 alphanumeric / `-` / `.`, 1..=253 chars, no leading/\
                 trailing hyphen",
            ));
        }
        return Ok(n.clone());
    }
    // Default: `<crate>-<type-kebab>` (matches the `#[container]` /
    // `#[workflow]` convention; see `make_argo_name`).
    let krate = crate::utils::crate_ns();
    Ok(format!("{}-{}", kebab(&krate), kebab(&ident.to_string())))
}

fn parse_attr<T: deluxe::ParseMetaItem + Default>(attr: TokenStream) -> Result<T, TokenStream> {
    if attr.is_empty() {
        return Ok(T::default());
    }
    deluxe::parse2::<T>(attr.into()).map_err(|e| e.into_compile_error().into())
}
