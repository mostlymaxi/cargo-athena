//! The `#[workflow]` proc-macro expansion.
//!
//! Wraps [`crate::analyze::analyze_workflow`] (DAG/steps lowering) +
//! [`crate::conditional::emit_synth`] (if-wrapper synthesis) +
//! [`crate::node_tokens::node_tokens`] (per-task `quote!` blocks) into
//! one `impl Template` for the user's unit-struct identity. Also emits
//! the never-run ghost ([`crate::ghost::ghost_fn`]) that gives rustc
//! arg/field/return type-checking over the analyzed body.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::{format_ident, quote};
use syn::{Expr, ItemFn, Path};

use crate::analyze::analyze_workflow;
use crate::attrs::{
    WorkflowArgs, inject_lower, lower_mutex_pairs, lower_toleration_args,
    mutexes_if_root_const_tokens, parse_attr, pod_gc_const_tokens, retry_strategy_tokens,
    secs_i64_tok, template_synchronization_tokens, tolerations_if_root_const_tokens,
    ttl_const_tokens,
};
use crate::conditional::emit_synth;
use crate::ghost::ghost_fn;
use crate::node_tokens::node_tokens;
use crate::utils::{
    check_yaml_safe_names, fn_args, is_artifact_ty, make_argo_name, scan_body, sig_shim, str_slice,
};

pub(crate) fn expand(attr: TokenStream, item: TokenStream) -> TokenStream {
    let func = syn::parse_macro_input!(item as ItemFn);
    // A `#[workflow]` body is statically analyzed (read), not executed
    // — `.await` is meaningless here, and an `async fn` would just
    // return a Future from the never-run body. Reject up front with a
    // pointed error rather than a confusing downstream type mismatch.
    if let Some(async_kw) = &func.sig.asyncness {
        return syn::Error::new_spanned(
            async_kw,
            "`#[workflow]` cannot be `async fn` — workflow bodies are \
             statically analyzed, not executed. Use a regular `fn`. \
             (Only `#[container]` bodies actually run; they may be \
             `async fn` with the `cargo-athena` `tokio` feature.)",
        )
        .to_compile_error()
        .into();
    }
    let cfg: WorkflowArgs = match parse_attr(attr) {
        Ok(c) => c,
        Err(e) => return e,
    };
    if let Err(e) = check_yaml_safe_names(&func, &cfg.name) {
        return e;
    }
    let steps_mode = cfg.steps.is_set();

    let vis = &func.vis;
    let ident = func.sig.ident.clone();
    let rust_name = ident.to_string();
    let argo_name = make_argo_name(&cfg.name, &rust_name);

    let scan = scan_body(&func);
    if !scan.host_paths.is_empty()
        || !scan.in_artifacts.is_empty()
        || !scan.out_artifacts.is_empty()
        || !scan.secrets.is_empty()
    {
        return syn::Error::new_spanned(
            &func.sig.ident,
            "`host!`/`load_artifact*!`/`save_artifact*!`/`secret*!` cannot be used \
             in a #[workflow]: a workflow is a DAG, not a pod. Declare them in the \
             #[container] (or a #[fragment] it calls) that runs in-cluster.",
        )
        .to_compile_error()
        .into();
    }
    let (nodes, output_task, synths) = match analyze_workflow(&func, &argo_name) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let node_blocks: Vec<_> = nodes.iter().map(|n| node_tokens(n, steps_mode)).collect();
    // Synthesized `if` wrappers/arms, emitted flat as sibling items.
    let synth_items: Vec<TokenStream2> = synths.iter().map(emit_synth).collect();

    // A returned value bubbles the terminal task's `return` up as this
    // template's own `outputs.parameters.return` (or
    // `outputs.artifacts.return` when the workflow returns
    // `Artifact<T>`), so a parent can wire either flavor to a sub-
    // workflow exactly like a container.
    let workflow_returns_artifact = matches!(
        &func.sig.output,
        syn::ReturnType::Type(_, t) if is_artifact_ty(t),
    );
    let outputs_tokens = match &output_task {
        Some(t) => {
            let scope = if steps_mode { "steps" } else { "tasks" };
            if workflow_returns_artifact {
                let refstr = format!("{{{{{scope}.{t}.outputs.artifacts.return}}}}");
                quote! {
                    outputs: ::core::option::Option::Some(
                        ::cargo_athena::api::Outputs {
                            artifacts: ::std::vec![
                                ::cargo_athena::api::Artifact {
                                    name: "return".to_string(),
                                    from: #refstr.to_string(),
                                    ..::core::default::Default::default()
                                }
                            ],
                            ..::core::default::Default::default()
                        }
                    ),
                }
            } else {
                let refstr = format!("{{{{{scope}.{t}.outputs.parameters.return}}}}");
                quote! {
                    outputs: ::core::option::Option::Some(
                        ::cargo_athena::api::Outputs {
                            parameters: ::std::vec![
                                ::cargo_athena::api::Parameter {
                                    name: "return".to_string(),
                                    value_from: ::core::option::Option::Some(
                                        ::cargo_athena::api::ValueFrom {
                                            parameter: #refstr.to_string(),
                                            ..::core::default::Default::default()
                                        }
                                    ),
                                    ..::core::default::Default::default()
                                }
                            ],
                            ..::core::default::Default::default()
                        }
                    ),
                }
            }
        }
        None => quote! {},
    };
    let output_kind_tok = if workflow_returns_artifact {
        quote! { ::cargo_athena::IoKind::Artifact }
    } else {
        quote! { ::cargo_athena::IoKind::Parameter }
    };

    // Distinct callee + hook-template + on_exit paths -> recurse their
    // `collect` (force-links them so they're emitted like any callee).
    let mut seen_callees = std::collections::HashSet::new();
    let callee_paths: Vec<&Path> = nodes
        .iter()
        .flat_map(|n| std::iter::once(&n.callee).chain(n.hooks.iter().map(|h| &h.template)))
        .chain(cfg.on_exit_if_root.iter())
        .filter(|p| seen_callees.insert(quote!(#p).to_string()))
        .collect();

    let wf_args = fn_args(&func);
    let arg_names: Vec<String> = wf_args.iter().map(|(i, _)| i.to_string()).collect();
    let inputs_slice = str_slice(&arg_names);
    // Stringified arg types, parallel to INPUTS — `workflow ls` shows
    // them (same as `container ls`).
    let arg_type_strs: Vec<String> = wf_args
        .iter()
        .map(|(_, t)| quote!(#t).to_string())
        .collect();
    let input_types_slice = str_slice(&arg_type_strs);
    // Per-arg I/O kind: `Artifact<T>` workflow inputs land on
    // `inputs.artifacts[]` (so a parent passes them through
    // `arguments.artifacts.from`); everything else stays on
    // `inputs.parameters[]`. Stamped into `INPUT_KINDS` so the parent
    // workflow's per-task wiring reads it at emit-time.
    let arg_is_artifact: Vec<bool> = wf_args.iter().map(|(_, t)| is_artifact_ty(t)).collect();
    let input_kind_toks: Vec<TokenStream2> = arg_is_artifact
        .iter()
        .map(|a| {
            if *a {
                quote! { ::cargo_athena::IoKind::Artifact }
            } else {
                quote! { ::cargo_athena::IoKind::Parameter }
            }
        })
        .collect();
    let param_input_names: Vec<&String> = arg_names
        .iter()
        .zip(arg_is_artifact.iter())
        .filter_map(|(n, a)| if *a { None } else { Some(n) })
        .collect();
    let art_input_names: Vec<&String> = arg_names
        .iter()
        .zip(arg_is_artifact.iter())
        .filter_map(|(n, a)| if *a { Some(n) } else { None })
        .collect();
    let param_input_names_strs: Vec<String> =
        param_input_names.iter().map(|s| (*s).clone()).collect();
    let art_input_names_strs: Vec<String> = art_input_names.iter().map(|s| (*s).clone()).collect();
    let inputs_tokens = if arg_names.is_empty() {
        quote! { ::core::option::Option::None }
    } else {
        quote! {
            ::core::option::Option::Some(::cargo_athena::api::Inputs {
                parameters: ::std::vec![
                    #( ::cargo_athena::api::Parameter {
                        name: #param_input_names_strs.to_string(),
                        ..::core::default::Default::default()
                    } ),*
                ],
                artifacts: ::std::vec![
                    #( ::cargo_athena::api::Artifact {
                        name: #art_input_names_strs.to_string(),
                        ..::core::default::Default::default()
                    } ),*
                ],
            })
        }
    };

    // `boundary_node_selector` — literal keys *and* values (no
    // injection — workflow attrs have no args to inject from). Set on
    // this dag/steps template's `Template.NodeSelector`. Argo uses it
    // as the *boundary* fallback only for pods whose IMMEDIATE
    // enclosing dag/steps is this template — does NOT cascade through
    // nested sub-workflows. For "every pod in the run", use
    // `node_selector_if_root` (below).
    let ns_keys: Vec<&String> = cfg.boundary_node_selector.keys().collect();
    let ns_vals: Vec<&String> = cfg.boundary_node_selector.values().collect();
    let node_selector_tokens = quote! {
        node_selector: {
            let mut __ns = ::std::collections::BTreeMap::new();
            #( __ns.insert(
                #ns_keys.to_string(), #ns_vals.to_string()); )*
            __ns
        },
    };
    // `node_selector_if_root` — Argo `WorkflowSpec.NodeSelector`,
    // root-only, applies to every pod that doesn't have a template- or
    // boundary-level override. Emitted as the `NODE_SELECTOR_IF_ROOT`
    // trait const → Collector → per-WT spec post-pass (mirrors
    // `TTL`/`POD_GC`/`ACTIVE_DEADLINE_IF_ROOT`).
    //
    // Values: lone string literals are verbatim; `"lit" + arg` /
    // `"lit" + arg.field` lowers to `{{=fromJSON(workflow.parameters[..])}}`
    // — root-scoped substitution (the only form Argo resolves at
    // WorkflowSpec scope on v4.0.5; `inputs.parameters` is inert here).
    // `_if_root` semantic ⇒ injection is safe by construction: this attr
    // is inert when the WT is `templateRef`'d, and the workflow's own
    // args coincide with `workflow.parameters` when submitted as root.
    let argset: std::collections::HashSet<String> = arg_names.iter().cloned().collect();
    let mut wf_inject_ops: Vec<Expr> = Vec::new();
    let nsi_keys: Vec<&String> = cfg.node_selector_if_root.keys().collect();
    let nsi_vals: Vec<String> = match cfg
        .node_selector_if_root
        .values()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut wf_inject_ops,
                "workflow.parameters",
                "workflow",
            )
        })
        .collect::<syn::Result<Vec<_>>>()
    {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let node_selector_if_root_tok = quote! {
        &[ #( (#nsi_keys, #nsi_vals) ),* ]
    };
    // `mutexes = [{ name, namespace }, …]` (template-level) /
    // `mutexes_if_root = […]` (root-only). Lowers each operand via
    // `inject_lower` against the right scope; injection operands flow
    // into `wf_inject_ops` so the existing `__athena_inject_check_<fn>`
    // shim covers them too:
    //
    // * Template scope = `inputs.parameters` (per-dag-invocation
    //   substitution — empirically safe at `Template.synchronization`,
    //   no nodeSelector-style boundary-copy footgun).
    // * `_if_root` scope = `workflow.parameters` (the only form Argo
    //   resolves at `WorkflowSpec` scope).
    let mutex_pairs: Vec<(String, String)> = match lower_mutex_pairs(
        &cfg.mutexes,
        &argset,
        &mut wf_inject_ops,
        "inputs.parameters",
        "workflow",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let mutex_ifroot_pairs: Vec<(String, String)> = match lower_mutex_pairs(
        &cfg.mutexes_if_root,
        &argset,
        &mut wf_inject_ops,
        "workflow.parameters",
        "workflow",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let synchronization_tok = template_synchronization_tokens(&mutex_pairs);
    let mutexes_if_root_tok = mutexes_if_root_const_tokens(&mutex_ifroot_pairs);
    // `tolerations_if_root` — root-only WorkflowSpec scope (the only
    // form Argo resolves at WfSpec). Workflows have no template-level
    // tolerations attr in v1 (boundary-tier deferred).
    let tol_ifroot_pairs = match lower_toleration_args(
        &cfg.tolerations_if_root,
        &argset,
        &mut wf_inject_ops,
        "workflow.parameters",
        "workflow",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let tolerations_if_root_tok = tolerations_if_root_const_tokens(&tol_ifroot_pairs);
    // `boundary_tolerations` — `Template.Tolerations` on this dag/steps
    // template, inherited by child pods that don't set their own.
    // Literal-only (`BoundaryTolerationArg` has plain String/i64
    // fields): per-arg injection here lowers to `workflow.parameters`
    // which surprise-resolves against the SUBMITTED ROOT when this WT
    // is `templateRef`'d. Same boundary-tier rationale as
    // `boundary_node_selector`.
    let boundary_tol_tokens = {
        let entries = cfg.boundary_tolerations.iter().map(|t| {
            let key = &t.key;
            let op = &t.operator;
            let val = &t.value;
            let eff = &t.effect;
            let secs = t.toleration_seconds;
            let secs_tok = if secs == 0 {
                quote! { ::core::option::Option::None }
            } else {
                quote! { ::core::option::Option::Some(#secs) }
            };
            quote! {
                ::cargo_athena::api::Toleration {
                    key: #key.to_string(),
                    operator: #op.to_string(),
                    value: #val.to_string(),
                    effect: #eff.to_string(),
                    toleration_seconds: #secs_tok,
                }
            }
        });
        quote! { ::std::vec![ #( #entries ),* ] }
    };
    // `boundary_affinity` — opaque YAML/JSON string for `Template
    // .Affinity` on this dag/steps template. Literal-only (same
    // boundary-tier rationale). Parsed at emit time via serde_norway.
    let boundary_affinity_tok = match &cfg.boundary_affinity {
        Some(s) => quote! {
            ::core::option::Option::Some(
                ::cargo_athena::serde_norway::from_str(#s)
                    .unwrap_or_else(|e| ::core::panic!(
                        "boundary_affinity: invalid YAML/JSON: {e}"
                    ))
            )
        },
        None => quote! { ::core::option::Option::None },
    };
    // `affinity_if_root` — opaque YAML/JSON string. Lowered against
    // `workflow.parameters` so a `"lit" + arg` chain inside the YAML
    // resolves at WfSpec scope.
    let affinity_if_root_s = match cfg
        .affinity_if_root
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut wf_inject_ops,
                "workflow.parameters",
                "workflow",
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
    // `pod_spec_patch_if_root = "..."` — root-only
    // `WorkflowSpec.PodSpecPatch`. Lowered against `workflow.parameters`;
    // the workflow-tier scope for injection (the only form Argo
    // resolves at WorkflowSpec).
    let pod_spec_patch_if_root_s = match cfg
        .pod_spec_patch_if_root
        .as_ref()
        .map(|e| {
            inject_lower(
                e,
                &argset,
                &mut wf_inject_ops,
                "workflow.parameters",
                "workflow",
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
    // `image_pull_secrets_if_root` — literal Secret names, flat list
    // into the `IMAGE_PULL_SECRETS_IF_ROOT` const (workflow-spec scope).
    let image_pull_secrets_if_root_tok = {
        let names = &cfg.image_pull_secrets_if_root;
        quote! { &[ #( #names ),* ] }
    };
    // Type-guard for every injected workflow operand — same shape as the
    // container guard (a hidden never-run fn asserting `Injectable`).
    let wf_inject_check = if wf_inject_ops.is_empty() {
        quote! {}
    } else {
        let orig_inputs = &func.sig.inputs;
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
                    __athena_assert(&#wf_inject_ops);
                )*
            }
        }
    };
    // `annotations` — literal keys + values, lands on
    // `Template.metadata.annotations` of the dag/steps template. Same
    // built-then-checked shape as the container side: keep
    // `metadata: None` (skip-serialized) when the attr is absent so
    // existing goldens stay byte-identical.
    let ann_keys: Vec<&String> = cfg.annotations.keys().collect();
    let ann_vals: Vec<&String> = cfg.annotations.values().collect();
    let metadata_tokens = quote! {
        metadata: {
            let mut __ann: ::std::collections::BTreeMap<
                ::std::string::String,
                ::std::string::String,
            > = ::std::collections::BTreeMap::new();
            #( __ann.insert(#ann_keys.to_string(), #ann_vals.to_string()); )*
            if __ann.is_empty() {
                ::core::option::Option::None
            } else {
                ::core::option::Option::Some(::cargo_athena::api::ObjectMeta {
                    annotations: __ann,
                    ..::core::default::Default::default()
                })
            }
        },
    };

    // Template-level `retryStrategy` / `timeout` (real WT only — never
    // re-stamped on synthetic `if` wrapper/arm templates).
    let retry_tok = match retry_strategy_tokens(&cfg.retry, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let active_deadline_if_root_tok = match secs_i64_tok(
        &cfg.active_deadline_if_root,
        ident.span(),
        "active_deadline_if_root",
    ) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    // A `#[workflow]` lowers to a dag/steps template, where Argo applies
    // neither `Template.timeout` nor `Template.activeDeadlineSeconds`
    // (both documented no-ops on dag/steps) — so a workflow template
    // only carries `retryStrategy`. The whole-workflow runtime cap is
    // `active_deadline_if_root` (WorkflowSpec-scoped, below).
    let retry_tokens = quote! {
        retry_strategy: #retry_tok,
    };
    // WorkflowSpec-scoped `ttlStrategy` / `podGC` trait consts (stamped
    // per-WT by `Collector` like `ON_EXIT`; never on synthetic `if`
    // wrapper/arm templates — `emit_synth` omits these consts).
    let ttl_tok = match ttl_const_tokens(&cfg.ttl_if_root, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };
    let podgc_tok = match pod_gc_const_tokens(&cfg.pod_gc_if_root, ident.span()) {
        Ok(v) => v,
        Err(e) => return e.to_compile_error().into(),
    };

    // Default = `dag:` (parallel by data-deps). `#[workflow(steps)]` =
    // `steps:` (one sequential group per statement).
    let build_body = if steps_mode {
        quote! {
            let mut __steps: ::std::vec::Vec<
                ::std::vec::Vec<::cargo_athena::api::DagTask>,
            > = ::std::vec::Vec::new();
            #( #node_blocks )*
            ::cargo_athena::api::Template {
                name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                #metadata_tokens
                inputs: #inputs_tokens,
                steps: __steps,
                #node_selector_tokens
                #retry_tokens
                #outputs_tokens
                synchronization: #synchronization_tok,
                tolerations: #boundary_tol_tokens,
                affinity: #boundary_affinity_tok,
                ..::core::default::Default::default()
            }
        }
    } else {
        quote! {
            let mut __tasks: ::std::vec::Vec<::cargo_athena::api::DagTask> =
                ::std::vec::Vec::new();
            #( #node_blocks )*
            ::cargo_athena::api::Template {
                name: <Self as ::cargo_athena::Template>::ARGO_NAME.to_string(),
                #metadata_tokens
                inputs: #inputs_tokens,
                dag: ::core::option::Option::Some(
                    ::cargo_athena::api::DagTemplate { tasks: __tasks }),
                #node_selector_tokens
                #retry_tokens
                #outputs_tokens
                synchronization: #synchronization_tok,
                tolerations: #boundary_tol_tokens,
                affinity: #boundary_affinity_tok,
                ..::core::default::Default::default()
            }
        }
    };

    // `#[workflow(on_exit_if_root = t)]` -> Template::ON_EXIT (emit
    // puts it on this template's own spec.hooks.exit; Argo fires it
    // only for the submitted workflow).
    let on_exit_const = match &cfg.on_exit_if_root {
        Some(p) => quote! {
            const ON_EXIT: ::core::option::Option<&'static str> =
                ::core::option::Option::Some(
                    <#p as ::cargo_athena::Template>::ARGO_NAME,
                );
        },
        None => quote! {},
    };

    let sig_block = sig_shim(&ident, &func);
    let ghost = ghost_fn(&func);

    let expanded = quote! {
        // The body is compiled to Argo; the public identity is a type.
        #[allow(non_camel_case_types)]
        #vis struct #ident;

        // Typed signature shim + never-run ghost: rustc type-checks the
        // analyzed body's data flow (arg/field/return types) even though
        // the body itself isn't compiled. The inject-check shim asserts
        // any `node_selector_if_root` operand is `Injectable`.
        #sig_block
        #ghost
        #wf_inject_check

        // Synthesized `if` wrappers + arm sub-workflows (force-linked via
        // this workflow's `collect`, since its `if` nodes name them).
        #( #synth_items )*

        impl ::cargo_athena::Template for #ident {
            const ARGO_NAME: &'static str = #argo_name;
            const INPUTS: &'static [&'static str] = #inputs_slice;
            const INPUT_TYPES: &'static [&'static str] = #input_types_slice;
            const INPUT_KINDS: &'static [::cargo_athena::IoKind] = &[
                #( #input_kind_toks ),*
            ];
            const OUTPUT_KIND: ::cargo_athena::IoKind = #output_kind_tok;
            const KIND: ::cargo_athena::TemplateKind =
                ::cargo_athena::TemplateKind::Workflow;
            #on_exit_const
            const TTL: ::core::option::Option<::cargo_athena::api::TtlStrategy> = #ttl_tok;
            const POD_GC: ::core::option::Option<&'static str> = #podgc_tok;
            const ACTIVE_DEADLINE_IF_ROOT: ::core::option::Option<i64> =
                #active_deadline_if_root_tok;
            const NODE_SELECTOR_IF_ROOT: &'static [(&'static str, &'static str)] =
                #node_selector_if_root_tok;
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

            fn build(_ctx: &::cargo_athena::BuildCtx)
                -> ::cargo_athena::api::Template
            {
                #build_body
            }

            fn collect(__out: &mut ::cargo_athena::Collector) {
                if !__out.enter(<Self as ::cargo_athena::Template>::ARGO_NAME) {
                    return;
                }
                __out.add::<Self>();
                #(
                    <#callee_paths as ::cargo_athena::Template>::collect(__out);
                )*
            }
        }
    };
    expanded.into()
}
