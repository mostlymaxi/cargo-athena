//! The `#[container]` proc-macro expansion.
//!
//! Turns a `#[container] fn foo(args) -> R { body }` into:
//! - A hidden inherent fn `__cargo_athena_impl_foo` carrying the real body.
//! - A unit struct `foo` (the wormhole identity).
//! - An `impl Template for foo` with the runtime dispatcher
//!   (deserialize inputs → call body → serialize output) and the emit
//!   builder (the Argo `Template` with the arch-resolving bootstrap,
//!   `host!`/artifact/secret env, all the spec-scoped attrs).

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{Expr, ItemFn};

use crate::attrs::{
    ContainerArgs, inject_lower, lower_mutex_pairs, lower_toleration_args,
    mutexes_if_root_const_tokens, parse_attr, pod_gc_const_tokens, retry_strategy_tokens,
    secs_i32_tok, secs_i64_tok, template_synchronization_tokens, template_tolerations_tokens,
    timeout_tokens, tolerations_if_root_const_tokens, ttl_const_tokens,
};
use crate::utils::{
    check_yaml_safe_names, fn_arg_injects, fn_args, func_without_inject_args, is_artifact_ty,
    make_argo_name, scan_body, secret_slice_tokens, sig_shim, str_slice, strip_inject_attrs,
    with_host_rewritten,
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
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
    // Strip the `#[inject(...)]` per-arg attrs so the re-emitted fn
    // compiles cleanly under rustc (the attr is athena-private).
    strip_inject_attrs(&mut impl_fn);
    let impl_ident = format_ident!("__cargo_athena_impl_{}", ident);
    impl_fn.sig.ident = impl_ident.clone();
    impl_fn.vis = syn::Visibility::Inherited;
    // Caller-visible signature (used by sig_shim + ghost): drop the
    // `#[inject(...)]`-attributed args entirely. Workflow bodies call
    // this template without those args; Argo fills them in-pod from
    // the user's raw expression in `container.args[]`.
    let caller_func = func_without_inject_args(&func);

    // `async fn` bodies: wrap the call in our `block_on`. The hidden
    // impl-fn keeps its `async` (returns a Future); `run` is sync, so
    // it builds a single-thread tokio runtime once per invocation.
    // Requires the `tokio` feature on cargo-athena; without it the
    // `__async` path doesn't exist and the user gets a missing-module
    // error pointing at the feature.
    let is_async = func.sig.asyncness.is_some();

    let args = fn_args(&func);
    let arg_idents: Vec<_> = args.iter().map(|(i, _)| i.clone()).collect();
    let arg_types: Vec<_> = args.iter().map(|(_, t)| t.clone()).collect();
    // Parallel to `args`: `Some("<argo expr>")` for `#[inject(...)]`-
    // attributed args (filled by Argo at run time, NOT declared as
    // inputs.parameters), `None` for normal parameter args.
    let arg_injects: Vec<Option<String>> = fn_arg_injects(&func);
    // The `run`-body call expression. Sync = bare call; async = wrap
    // the returned Future in `block_on` (drives the body on a fresh
    // current-thread runtime — see `cargo_athena::__async`).
    let call_expr = if is_async {
        quote! {
            ::cargo_athena::__async::block_on(
                #impl_ident( #( #arg_idents ),* )
            )
        }
    } else {
        quote! { #impl_ident( #( #arg_idents ),* ) }
    };
    let arg_names: Vec<String> = arg_idents.iter().map(|i| i.to_string()).collect();
    let inputs_slice = str_slice(&arg_names);
    // Stringified arg types, parallel to INPUTS — `container emulate`
    // type-checks supplied params against these before launching.
    let arg_type_strs: Vec<String> = arg_types.iter().map(|t| quote!(#t).to_string()).collect();
    let input_types_slice = str_slice(&arg_type_strs);

    // Per-arg I/O kind: an `Artifact<T>` parameter or return type means
    // the value flows via Argo's `outputs.artifacts` / `arguments
    // .artifacts.from` channel (S3-backed). Everything else stays on the
    // inline-parameter path. Used to (a) stamp the `INPUT_KINDS` /
    // `OUTPUT_KIND` consts for the workflow side to read at emit-time,
    // (b) split inputs into `inputs.parameters` vs `inputs.artifacts`,
    // (c) branch the `run()` body's deserialization per arg (argv vs
    // file), (d) emit `outputs.parameters.return` vs
    // `outputs.artifacts.return`, and (e) drive the bootstrap's
    // CARGO_ATHENA_OUTPUT path.
    let arg_is_artifact: Vec<bool> = arg_types.iter().map(|t| is_artifact_ty(t)).collect();
    let output_is_artifact = matches!(
        &func.sig.output,
        syn::ReturnType::Type(_, t) if is_artifact_ty(t),
    );
    let output_kind_tok = if output_is_artifact {
        quote! { ::cargo_athena::IoKind::Artifact }
    } else {
        quote! { ::cargo_athena::IoKind::Parameter }
    };
    let input_kind_toks: Vec<proc_macro2::TokenStream> = arg_is_artifact
        .iter()
        .map(|a| {
            if *a {
                quote! { ::cargo_athena::IoKind::Artifact }
            } else {
                quote! { ::cargo_athena::IoKind::Parameter }
            }
        })
        .collect();
    // Parameter-only args declared as `inputs.parameters[]`: non-
    // artifact AND non-inject. Artifact args travel via files;
    // `#[inject(...)]` args are filled by Argo from a raw expression
    // and never declared as inputs.parameters.
    let param_only_names_strs: Vec<String> = arg_names
        .iter()
        .zip(arg_is_artifact.iter())
        .zip(arg_injects.iter())
        .filter_map(|((n, art), inj)| {
            if *art || inj.is_some() {
                None
            } else {
                Some(n.clone())
            }
        })
        .collect();
    // Pre-formed positional argv elements (passed to
    // `container_delivery`). One element per non-artifact arg, in fn
    // declaration order. Param args contribute
    // `{{inputs.parameters.<n>}}`; inject args contribute the user's
    // raw Argo expression verbatim (athena does NOT munge it — the
    // user signs for any quoting/JSON wrapping needed for the target
    // arg type).
    let argv_elements: Vec<String> = arg_names
        .iter()
        .zip(arg_is_artifact.iter())
        .zip(arg_injects.iter())
        .filter_map(|((n, art), inj)| {
            if *art {
                return None;
            }
            Some(match inj {
                Some(expr) => expr.clone(),
                None => format!("{{{{inputs.parameters.{n}}}}}"),
            })
        })
        .collect();

    // Per-arg `run()` body deserialization statement. Parameter args
    // read from `__argv` (positional, in PARAMETER-only order);
    // artifact args read from `/athena/in/<name>` (one file per arg,
    // written by Argo's input-artifact tar+untar). The split lets a
    // container mix the two kinds without messing up argv indexing.
    let mut param_idx: usize = 0;
    let arg_deser_stmts: Vec<proc_macro2::TokenStream> = arg_idents
        .iter()
        .zip(arg_types.iter())
        .zip(arg_names.iter())
        .zip(arg_is_artifact.iter())
        .map(|(((ident, ty), name), is_art)| {
            if *is_art {
                quote! {
                    let #ident: #ty = {
                        let __path = ::std::format!(
                            "{}/{}",
                            ::cargo_athena::ATHENA_INPUT_ARTIFACT_DIR,
                            #name,
                        );
                        let __file = ::std::fs::File::open(&__path)
                            .unwrap_or_else(|e| ::std::panic!(
                                "open artifact input `{}` at {}: {}",
                                #name, __path, e
                            ));
                        ::cargo_athena::serde_json::from_reader(__file)
                            .unwrap_or_else(|e| ::std::panic!(
                                "deserialize artifact input `{}`: {}",
                                #name, e
                            ))
                    };
                }
            } else {
                let pi = param_idx;
                param_idx += 1;
                quote! {
                    let __raw: &::std::primitive::str = &__argv[#pi];
                    let #ident: #ty =
                        ::cargo_athena::serde_json::from_str(__raw)
                            .or_else(|_| {
                                ::cargo_athena::serde_json::from_value(
                                    ::cargo_athena::serde_json::Value::String(__raw.to_string())
                                )
                            })
                            .expect(concat!(
                                "deserialize container input `", #name, "`"
                            ));
                }
            }
        })
        .collect();
    let param_arg_count = param_idx;
    // The list of artifact-typed arg names; each becomes its own
    // `inputs.artifacts[]` entry with path `/athena/in/<name>`. The
    // run() body reads + deserializes the file at that path.
    let art_arg_names: Vec<&String> = arg_names
        .iter()
        .zip(arg_is_artifact.iter())
        .filter_map(|(n, a)| if *a { Some(n) } else { None })
        .collect();
    let art_arg_names_strs: Vec<String> = art_arg_names.iter().map(|s| (*s).clone()).collect();

    // The producer's return-side outputs block, branched at macro
    // time on the return type's kind. Parameter: today's
    // `outputs.parameters.return.valueFrom.path` (read from
    // ATHENA_RESULT_FILE). Artifact: `outputs.artifacts.return` with
    // an S3 location keyed off `{{pod.name}}` (node-unique within a
    // workflow, workflow-unique because pod names include the wf
    // name; verified live on Argo v4.0.5). No `archive:` so Argo's
    // executor default (tar+gzip on producer, untar on consumer)
    // round-trips a single file through S3 transparently to user code.
    let return_param_block = quote! {
        parameters: ::std::vec![ ::cargo_athena::api::Parameter {
            name: "return".to_string(),
            value_from: ::core::option::Option::Some(
                ::cargo_athena::api::ValueFrom {
                    path: ::cargo_athena::ATHENA_RESULT_FILE.to_string(),
                    ..::core::default::Default::default()
                }
            ),
            ..::core::default::Default::default()
        } ],
    };
    let return_artifact_block = quote! {
        parameters: ::std::vec![],
    };
    let return_artifact_extra = if output_is_artifact {
        Some(quote! {
            {
                let __s3 = &__ctx.config().artifact_repository.s3;
                __out_artifacts.push(::cargo_athena::api::Artifact {
                    name: "return".to_string(),
                    path: ::cargo_athena::ATHENA_RESULT_ARTIFACT_FILE.to_string(),
                    s3: ::core::option::Option::Some(
                        ::cargo_athena::s3_loc(__s3, "{{pod.name}}/return")
                    ),
                    archive: ::core::option::Option::None,
                    mode: ::core::option::Option::None,
                    from: ::std::string::String::new(),
                });
            }
        })
    } else {
        None
    };
    let outputs_param_block = if output_is_artifact {
        return_artifact_block
    } else {
        return_param_block
    };
    let host_slice = str_slice(&scan.host_paths);
    let in_art_slice = str_slice(&scan.in_artifacts);
    let out_art_slice = str_slice(&scan.out_artifacts);
    let secret_slice = secret_slice_tokens(&scan.secrets);
    let callee_slice = str_slice(&scan.callees);
    // Lower the injectable attribute values (image / service_account /
    // node_selector values) — a string literal stays verbatim; a
    // `+`-concat injects args as `{{=fromJSON(inputs.parameters[..])}}`.
    let argset: std::collections::HashSet<String> = arg_names.iter().cloned().collect();
    let mut inject_ops: Vec<Expr> = Vec::new();
    let image_s = match cfg
        .image
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let sa_s = match cfg
        .service_account
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let ns_keys: Vec<&String> = cfg.node_selector.keys().collect();
    let ns_vals: Vec<String> = match cfg
        .node_selector
        .values()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .collect::<syn::Result<Vec<_>>>()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // `env = { "KEY" = "lit" + arg, … }`: extra container env entries.
    // Literal keys (BTreeMap key type), injectable values (same
    // lowering as image / node_selector / SA).
    let env_keys: Vec<&String> = cfg.env.keys().collect();
    let env_vals: Vec<String> = match cfg
        .env
        .values()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .collect::<syn::Result<Vec<_>>>()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // `annotations = { "k" = "lit" + arg, … }`: lands on
    // `Template.metadata.annotations`. Same shape as env.
    let ann_keys: Vec<&String> = cfg.annotations.keys().collect();
    let ann_vals: Vec<String> = match cfg
        .annotations
        .values()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .collect::<syn::Result<Vec<_>>>()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // `mutexes = [{ name, namespace }, …]` (template-level) /
    // `mutexes_if_root = […]` (root-only WorkflowSpec). Both lists go
    // through `inject_lower` so a literal stays verbatim and a
    // `"lit" + arg` chain becomes `{{=fromJSON(<scope>['arg'])}}`:
    //
    // * Template scope = `inputs.parameters` (per-step substitution —
    //   empirically safe on `Template.synchronization`, no nodeSelector-
    //   style boundary-copy footgun; proven on v4.0.5 2026-05-25).
    // * `_if_root` scope = `workflow.parameters` (the only form Argo
    //   resolves at `WorkflowSpec` scope).
    //
    // Operands flow into the existing `inject_ops` so the
    // `Injectable` type-check shim (built below) covers them too.
    let mutex_pairs: Vec<(String, String)> = match lower_mutex_pairs(
        &cfg.mutexes,
        &argset,
        &mut inject_ops,
        "inputs.parameters",
        "container",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let mutex_ifroot_pairs: Vec<(String, String)> = match lower_mutex_pairs(
        &cfg.mutexes_if_root,
        &argset,
        &mut inject_ops,
        "workflow.parameters",
        "container",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let synchronization_tok = template_synchronization_tokens(&mutex_pairs);
    let mutexes_if_root_tok = mutexes_if_root_const_tokens(&mutex_ifroot_pairs);
    // `tolerations = [...]` (template-level on this container's WT,
    // safe at template scope) + `tolerations_if_root = [...]` (root-
    // only, WfSpec scope). Keys/values/effect strings are injectable;
    // operator + toleration_seconds are literal.
    let tol_pairs = match lower_toleration_args(
        &cfg.tolerations,
        &argset,
        &mut inject_ops,
        "inputs.parameters",
        "container",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let tol_ifroot_pairs = match lower_toleration_args(
        &cfg.tolerations_if_root,
        &argset,
        &mut inject_ops,
        "workflow.parameters",
        "container",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let tolerations_tok = template_tolerations_tokens(&tol_pairs);
    let tolerations_if_root_tok = tolerations_if_root_const_tokens(&tol_ifroot_pairs);
    // `affinity = "<json|yaml>"` / `affinity_if_root = "..."` — opaque
    // YAML/JSON strings. No automatic injection (the user writes raw
    // `{{...}}` substitutions inside the YAML body if needed). Athena
    // does NOT model `apiv1.Affinity`'s deeply-nested schema.
    let affinity_s = match cfg
        .affinity
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let affinity_tok = match affinity_s {
        Some(s) => quote! {
            ::core::option::Option::Some(
                ::cargo_athena::serde_norway::from_str(#s)
                    .unwrap_or_else(|e| ::core::panic!(
                        "affinity: invalid YAML/JSON: {e}"
                    ))
            )
        },
        None => quote! { ::core::option::Option::None },
    };
    let affinity_if_root_s = match cfg
        .affinity_if_root
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "workflow.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let affinity_if_root_const_tok = match affinity_if_root_s {
        Some(s) => quote! { ::core::option::Option::Some(#s) },
        None => quote! { ::core::option::Option::None },
    };
    // `pod_spec_patch = "..."` — `Template.PodSpecPatch` on this
    // container's WT. Strategic-merge escape hatch; injectable via
    // `inputs.parameters` (safe at template scope per the same
    // pod-creation substitution pass that resolves the patch — proven
    // v4.0.5 2026-05-26, no nodeSelector-style boundary-copy footgun).
    let pod_spec_patch_s = match cfg
        .pod_spec_patch
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "inputs.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let pod_spec_patch_tok = match pod_spec_patch_s {
        Some(s) => quote! { ::core::option::Option::Some(#s.to_string()) },
        None => quote! { ::core::option::Option::None },
    };
    // `pod_spec_patch_if_root = "..."` — root-only
    // `WorkflowSpec.PodSpecPatch`. Lowered against `workflow.parameters`
    // (the only scope Argo resolves at WorkflowSpec).
    let pod_spec_patch_if_root_s = match cfg
        .pod_spec_patch_if_root
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut inject_ops,
                "workflow.parameters",
                "container",
            )
        })
        .transpose()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let pod_spec_patch_if_root_const_tok = match pod_spec_patch_if_root_s {
        Some(s) => quote! { ::core::option::Option::Some(#s) },
        None => quote! { ::core::option::Option::None },
    };
    // `image_pull_secrets_if_root = ["regcred", ...]` — literal Secret
    // names; flat list emitted into the `IMAGE_PULL_SECRETS_IF_ROOT`
    // const, stamped onto `spec.imagePullSecrets` by `Collector`.
    let image_pull_secrets_if_root_tok = {
        let names = &cfg.image_pull_secrets_if_root;
        quote! { &[ #( #names ),* ] }
    };
    // `host_mount = [{ host_path, mount_path, read_only }, …]`:
    // literal-only triples, threaded into `container_volumes` so the
    // dedup-against-`host!` lives in core.
    let host_mount_hosts: Vec<&String> = cfg.host_mount.iter().map(|h| &h.host_path).collect();
    let host_mount_mounts: Vec<&String> = cfg.host_mount.iter().map(|h| &h.mount_path).collect();
    let host_mount_ro: Vec<bool> = cfg.host_mount.iter().map(|h| h.read_only).collect();
    // `privileged = true` → K8s `securityContext.privileged: true`. Off
    // → emit `security_context: None` so the empty struct is
    // skip-serialized and existing goldens stay byte-identical.
    let security_context_tok = if cfg.privileged {
        quote! {
            ::core::option::Option::Some(::cargo_athena::api::SecurityContext {
                privileged: true,
            })
        }
    } else {
        quote! { ::core::option::Option::None }
    };
    let image_opt = match &image_s {
        Some(s) => quote! { ::core::option::Option::Some(#s) },
        None => quote! { ::core::option::Option::None },
    };
    let sa_opt = match &sa_s {
        Some(s) => quote! { ::core::option::Option::Some(#s) },
        None => quote! { ::core::option::Option::None },
    };
    // Template-level `retryStrategy` / `timeout`.
    let retry_tok = match retry_strategy_tokens(&cfg.retry, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let timeout_tok = match timeout_tokens(&cfg.timeout, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let pod_running_timeout_tok = match secs_i32_tok(
        &cfg.pod_running_timeout,
        ident.span(),
        "pod_running_timeout",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // Root-only WorkflowSpec runtime cap (stamped per-WT by `Collector`
    // like `ON_EXIT`/`TTL`; the only real whole-workflow timeout).
    let active_deadline_if_root_tok = match secs_i64_tok(
        &cfg.active_deadline_if_root,
        ident.span(),
        "active_deadline_if_root",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // WorkflowSpec-scoped `ttlStrategy` / `podGC` trait consts (stamped
    // per-WT by `Collector` like `ON_EXIT`).
    let ttl_tok = match ttl_const_tokens(&cfg.ttl_if_root, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let podgc_tok = match pod_gc_const_tokens(&cfg.pod_gc_if_root, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // Type-guard: every injected operand must be `Injectable`
    // (String/str/number) in the container's real arg types — a type
    // whose `serde_json` form round-trips to the obvious raw scalar.
    // Hidden, never called.
    let inject_check = if inject_ops.is_empty() {
        quote! {}
    } else {
        // Use the inject-stripped caller signature so the `#[inject(...)]`
        // per-arg attribute (athena-private) doesn't leak into rustc.
        let orig_inputs = &caller_func.sig.inputs;
        let chk = format_ident!("__athena_inject_check_{}", ident);
        quote! {
            #[doc(hidden)]
            #[allow(dead_code, unused, clippy::all)]
            fn #chk(#orig_inputs) {
                fn __athena_assert<T>(_: &T)
                where
                    T: ?Sized + ::cargo_athena::Injectable,
                {
                }
                #(
                    __athena_assert(&#inject_ops);
                )*
            }
        }
    };
    let vis = &func.vis;
    let sig_block = sig_shim(&ident, &caller_func);

    // `#[container(on_exit_if_root = t)]`: Template::ON_EXIT (fires when
    // this template is the submitted workflow) + force-link the handler.
    let (on_exit_const, on_exit_collect) = match &cfg.on_exit_if_root {
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

        // Never-run: each injected attribute operand must be `Display`.
        #inject_check

        // The importable identity: a type, not a fn.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        // Never-run typed signature shim (lets the #[workflow] ghost
        // type-check data flow through this template).
        #sig_block

        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo_name;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const INPUT_TYPES: &'static [&'static str] = #input_types_slice;
            const INPUT_KINDS: &'static [::cargo_athena::IoKind] = &[
                #( #input_kind_toks ),*
            ];
            const OUTPUT_KIND: ::cargo_athena::IoKind = #output_kind_tok;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Container;
            #on_exit_const
            const TTL: ::core::option::Option<::cargo_athena::api::TtlStrategy> = #ttl_tok;
            const POD_GC: ::core::option::Option<&'static str> = #podgc_tok;
            const ACTIVE_DEADLINE_IF_ROOT: ::core::option::Option<i64> =
                #active_deadline_if_root_tok;
            const MUTEXES_IF_ROOT: &'static [(&'static str, &'static str)] =
                #mutexes_if_root_tok;
            const TOLERATIONS_IF_ROOT:
                &'static [(&'static str, &'static str, &'static str, &'static str, i64)] =
                #tolerations_if_root_tok;
            const AFFINITY_IF_ROOT: ::core::option::Option<&'static str> =
                #affinity_if_root_const_tok;
            const POD_SPEC_PATCH_IF_ROOT: ::core::option::Option<&'static str> =
                #pod_spec_patch_if_root_const_tok;
            const IMAGE_PULL_SECRETS_IF_ROOT: &'static [&'static str] =
                #image_pull_secrets_if_root_tok;

            fn run(__argv: &[::std::string::String]) -> ::std::string::String
            {
                // Argv length must match the count of PARAMETER args
                // (artifact args arrive via files, not argv). A
                // mismatch means the binary and the WorkflowTemplate
                // that scheduled it were built from different source
                // revisions; fail loud rather than silently mis-bind
                // positions.
                let __expected = #param_arg_count;
                if __argv.len() != __expected {
                    panic!(
                        "{}: argv has {} arg(s), expected {} parameter(s) (binary <-> template version drift?)",
                        <Self as ::cargo_athena::Template>::ARGO_NAME,
                        __argv.len(),
                        __expected,
                    );
                }
                // Per-arg deserialization, branched by kind:
                // parameter args read from `__argv` (positional in
                // PARAMETER-only order); artifact args read a JSON file
                // at `/athena/in/<name>` written by Argo's input-
                // artifact tar+untar.
                #( #arg_deser_stmts )*
                let __out = #call_expr;
                // Serde-transparent `Artifact<T>` serializes as T, so
                // this write site stays kind-agnostic; the bootstrap
                // has already routed CARGO_ATHENA_OUTPUT to the right
                // file path (ATHENA_RESULT_FILE for parameter,
                // ATHENA_RESULT_ARTIFACT_FILE for artifact).
                ::cargo_athena::serde_json::to_string(&__out)
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
                    ::cargo_athena::container_volumes(
                        &__paths,
                        &[ #( (
                            #host_mount_hosts.to_string(),
                            #host_mount_mounts.to_string(),
                            #host_mount_ro,
                        ) ),* ],
                    );
                // Arbitrary user image + the arch-resolving bootstrap that
                // pulls & exec's the athena binary delivered as an artifact.
                // Only parameter args become positional argv; artifact
                // args arrive via files and aren't substituted into
                // `container.args`. The bootstrap also keys the
                // CARGO_ATHENA_OUTPUT path off OUTPUT_KIND so the
                // parameter/artifact write site picks the right file.
                let __d = ::cargo_athena::container_delivery(
                    __ctx,
                    &[ #( #argv_elements.to_string() ),* ],
                    #image_opt,
                    <Self as ::cargo_athena::Template>::OUTPUT_KIND,
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
                // `Artifact<T>`-typed fn args declared as Argo input
                // artifacts; each lands as a single file at
                // `/athena/in/<name>` (Argo's executor unpacks the
                // tar+gzip transparently).
                #(
                    __in_artifacts.push(::cargo_athena::api::Artifact {
                        name: #art_arg_names_strs.to_string(),
                        path: ::std::format!(
                            "{}/{}",
                            ::cargo_athena::ATHENA_INPUT_ARTIFACT_DIR,
                            #art_arg_names_strs,
                        ),
                        s3: ::core::option::Option::None,
                        archive: ::core::option::Option::None,
                        mode: ::core::option::Option::None,
                        from: ::std::string::String::new(),
                    });
                )*
                let mut __out_artifacts =
                    ::cargo_athena::artifact_outputs(__ctx, &__out_names);
                // Append `outputs.artifacts.return` when this container
                // returns an `Artifact<T>` (the S3-backed DAG-wired
                // path); a no-op otherwise.
                #return_artifact_extra
                // `#[container(annotations = { "k" = "lit" + arg, … })]`
                // lands on `Template.metadata.annotations`. Built-then-
                // checked: when the BTreeMap stays empty (no attr) we
                // emit `metadata: None`, so containers without
                // annotations keep byte-identical goldens.
                let mut __ann: ::std::collections::BTreeMap<
                    ::std::string::String,
                    ::std::string::String,
                > = ::std::collections::BTreeMap::new();
                #( __ann.insert(#ann_keys.to_string(), #ann_vals.to_string()); )*
                let __metadata = if __ann.is_empty() {
                    ::core::option::Option::None
                } else {
                    ::core::option::Option::Some(::cargo_athena::api::ObjectMeta {
                        annotations: __ann,
                        ..::core::default::Default::default()
                    })
                };
                ::cargo_athena::api::Template {
                    name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                    metadata: __metadata,
                    inputs: ::core::option::Option::Some(::cargo_athena::api::Inputs {
                        // Only parameter-typed args appear here;
                        // artifact-typed args are pushed into
                        // `__in_artifacts` above.
                        parameters: ::std::vec![
                            #( ::cargo_athena::api::Parameter {
                                name: #param_only_names_strs.to_string(),
                                ..::core::default::Default::default()
                            } ),*
                        ],
                        artifacts: __in_artifacts,
                    }),
                    outputs: ::core::option::Option::Some(::cargo_athena::api::Outputs {
                        // Named `return` (NOT `result`): `outputs.result`
                        // is Argo's script-stdout alias — a distinct
                        // thing. The return is on `outputs.parameters
                        // .return` (read from ATHENA_RESULT_FILE) by
                        // default; an `Artifact<T>` return lands on
                        // `outputs.artifacts.return` instead (pushed
                        // into `__out_artifacts` above).
                        #outputs_param_block
                        artifacts: __out_artifacts,
                    }),
                    container: ::core::option::Option::Some(::cargo_athena::api::Container {
                        image: __d.image,
                        command: __d.command,
                        args: __d.args,
                        security_context: #security_context_tok,
                        env: {
                            // Selector: the binary's entrypoint reads
                            // CARGO_ATHENA_TEMPLATE to pick which container
                            // body to run. Function params arrive as
                            // positional argv on `container.args` so
                            // Argo's automatic large-args offload kicks
                            // in for big values; env can't be offloaded.
                            let mut __env: ::std::vec::Vec<::cargo_athena::api::EnvVar> = ::std::vec![
                                ::cargo_athena::api::EnvVar {
                                    name: ::cargo_athena::CARGO_ATHENA_TEMPLATE_ENV.to_string(),
                                    value: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                                    ..::core::default::Default::default()
                                },
                            ];
                            // `#[container(env = { "K" = "lit" + arg, … })]` —
                            // literal keys + already-injection-lowered values
                            // (templated text or `{{=fromJSON(inputs.parameters['arg'])}}`).
                            #( __env.push(::cargo_athena::api::EnvVar {
                                name: #env_keys.to_string(),
                                value: #env_vals.to_string(),
                                ..::core::default::Default::default()
                            }); )*
                            // Resolved `secret!`/`secret_opt!` decls (own ∪ #[fragment]
                            // closure). Each becomes one `valueFrom.secretKeyRef` env;
                            // run-mode reads it back via `rt::secret_value(name, key)`.
                            for (__sn, __sk, __opt) in __ctx
                                .resolved_secrets(#secret_slice, #callee_slice)
                            {
                                __env.push(::cargo_athena::api::EnvVar {
                                    name: ::cargo_athena::secret_env_name(&__sn, &__sk),
                                    value_from: ::core::option::Option::Some(
                                        ::cargo_athena::api::EnvVarSource {
                                            secret_key_ref: ::core::option::Option::Some(
                                                ::cargo_athena::api::SecretKeySelector {
                                                    name: __sn,
                                                    key: __sk,
                                                    optional: __opt,
                                                },
                                            ),
                                        },
                                    ),
                                    ..::core::default::Default::default()
                                });
                            }
                            __env
                        },
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
                    retry_strategy: #retry_tok,
                    timeout: #timeout_tok,
                    active_deadline_seconds: #pod_running_timeout_tok,
                    synchronization: #synchronization_tok,
                    tolerations: #tolerations_tok,
                    affinity: #affinity_tok,
                    pod_spec_patch: #pod_spec_patch_tok,
                    ..::core::default::Default::default()
                }
            }

            fn collect(__out: &mut ::cargo_athena::Collector) {
                if !__out.enter(<Self as ::cargo_athena::Template>::ARGO_NAME) {
                    return;
                }
                __out.add::<Self>();
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
