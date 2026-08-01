//! Lower one [`Node`] to the `quote!` block that pushes a `DagTask` (or
//! `Vec<DagTask>` step group) into the workflow's `Template::build`.
//!
//! This is the only producer of the Argo `templateRef` + parameter +
//! dependency wiring. Called once per task by [`crate::workflow::expand`]
//! and once per arm-task by [`crate::conditional::emit_synth`].
//!
//! ## `arg_value`
//!
//! Every `Arg` variant lowers to an Argo template-substitution string +
//! an optional upstream task dep. That mapping lives in **one place**
//! ([`arg_value`]), and the two consumption sites (task args + hook
//! args) just wrap it with their respective push. Adding a new `Arg`
//! variant means a single match arm here — task and hook lowerings
//! pick it up together (no risk of one drifting behind the other), and
//! the helper is pure-string so its output is unit-testable without
//! macro expansion.
//!
//! Hooks never produce a DAG dep (they're fired by status, not data
//! flow), so the hook callsite drops the second tuple element.

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::analyze::{Arg, FanSrc, HookWhen, JsonSrc, Node};

/// `g` = task `t`'s scope node, nil-safe (hooks can see missing tasks).
fn task_node(scope: &str, t: &str) -> String {
    format!("let g = {scope}?.['{t}']; ")
}

/// `s` = `t`'s aggregated `return`, degraded to `"[]"` when the group
/// failed / was skipped / is absent. `p` can be the STRING `"[]"`
/// (all children failed), hence the `type` gate before indexing.
fn agg_value(scope: &str, t: &str) -> String {
    format!(
        "{}let p = g?.outputs?.parameters; \
         let r = (type(p) == \"map\" ? p?.['return'] : nil) ?? \"[]\"; \
         let s = r == \"\" || r == \"null\" ? \"[]\" : r; ",
        task_node(scope, t)
    )
}

/// Kind-aware re-norm: Argo re-stringifies scalar elements but keeps
/// object/array elements native — decode only the strings.
const RENORM: &str = "map(fromJSON(s), { type(#) == \"string\" ? fromJSON(#) : # })";

/// serde `Result::Err` arm for [`crate::analyze::Arg::RefFallible`] /
/// `FanAggFallible`. The `}\u{20}}` spacing is load-bearing: Argo ends
/// a `{{=…}}` tag at the first `}}` substring. `status` falls back to
/// "Failed": 3.6 group nodes have no status in scope, and this arm is
/// only reached when elements actually failed.
fn argo_err(exit_code: &str) -> String {
    format!("toJSON({{'Err': {{'status': g?.status ?? \"Failed\", 'exit_code': {exit_code}}} }})")
}

/// The Argo template-substitution string for `arg`, plus the upstream
/// task name to wire as a DAG dependency (`None` in steps mode — Argo's
/// `steps:` is sequential, order *is* the dep — and `None` for
/// `Lit`/`Input`/`Item`, which have no upstream task).
///
/// Pure string: every distinction Argo cares about (dotted vs bracketed
/// `tasks.X` / `tasks['X']`, the type-heterogeneous fan-out re-norm,
/// `{{item}}` vs `{{item.f}}`) is captured in the returned template
/// string. The two callers below wrap this in a `quote!` push of
/// `Parameter { value: Some(...) }`.
pub(crate) fn arg_value(arg: &Arg, steps: bool) -> (String, Option<String>) {
    let task_scope = if steps { "steps" } else { "tasks" };
    // Argo's `steps:` is sequential by source order; sibling refs there
    // don't carry a `dependencies` list at all.
    let dep = |t: &str| (!steps).then(|| t.to_string());
    match arg {
        // Already-JSON-encoded literal (Regime B — `expr_to_arg` ran
        // `serde_json::to_string` so every param value is uniform JSON).
        Arg::Lit(v) => (v.clone(), None),
        // Whole-binding consumption of an upstream task: the explicitly
        // declared `outputs.parameters.return` (NOT `outputs.result`,
        // which is Argo's script-stdout alias and only exists for
        // container/script templates — a sub-workflow's return wouldn't
        // resolve through it). Dotted form here is fine because the
        // ref isn't inside an expr-template.
        Arg::Ref(t) => (
            format!("{{{{{task_scope}.{t}.outputs.parameters.return}}}}"),
            dep(t),
        ),
        Arg::Input(n) => (format!("{{{{inputs.parameters.{n}}}}}"), None),
        // `a.b.c` field access uses bracket form (`tasks['x']`,
        // `outputs.parameters['return']`) so hyphenated kebab task names
        // and the keyword `return` resolve. `toJSON(fromJSON(..))` is
        // the universal-safe round-trip — athena's run-side is
        // `from_str` else `String`, so this reconstructs every field
        // kind (quoted strings, numbers, nested structs/arrays).
        Arg::Json { src, path } => {
            let acc: String = path.iter().map(|f| format!("['{f}']")).collect();
            let (refexpr, d) = match src {
                JsonSrc::Task(t) => (
                    format!("{task_scope}['{t}'].outputs.parameters['return']"),
                    dep(t),
                ),
                JsonSrc::Input(n) => (format!("inputs.parameters['{n}']"), None),
            };
            (format!("{{{{=toJSON(fromJSON({refexpr}){acc})}}}}"), d)
        }
        // The fan-out closure param: `{{item}}` / `{{item.f.g}}`. Argo
        // binds `item` per iteration of this task's `withParam`. No dep
        // (the iteration source is the `withParam` itself; the fan_out
        // node's own `__deps.push` covers it).
        Arg::Item { path } => {
            let mut v = String::from("{{item");
            for f in path {
                v.push('.');
                v.push_str(f);
            }
            v.push_str("}}");
            (v, None)
        }
        // Fan-out aggregate: guarded lookup + kind-aware re-norm.
        Arg::FanAgg(t) => (
            format!("{{{{={}toJSON({RENORM})}}}}", agg_value(task_scope, t)),
            dep(t),
        ),
        // `.continue_on` binding: serde-tagged `Result<T, ArgoError>`.
        Arg::RefFallible(t) => (
            format!(
                "{{{{={}let c = g?.outputs?.exitCode; \
                 g?.status == \"Succeeded\" \
                 ? toJSON({{'Ok': fromJSON(g?.outputs?.parameters?.['return'] ?? \"null\")}}) \
                 : {err}}}}}",
                task_node(task_scope, t),
                err = argo_err("c == nil ? nil : int(c)")
            ),
            dep(t),
        ),
        // `.continue_on` fan-out: all-or-nothing `Result<Vec<T>, _>`.
        // Ok iff the aggregate is as long as the source list (Argo
        // aggregates succeeded elements only; group `status` is absent
        // from scope on 3.6, so completeness is judged by data alone).
        Arg::FanAggFallible { task, src } => {
            let src_val = match src {
                FanSrc::Task(p) => {
                    format!("{task_scope}?.['{p}']?.outputs?.parameters?.['return']")
                }
                FanSrc::Input(i) => format!("inputs?.parameters?.['{i}']"),
            };
            (
                format!(
                    "{{{{={}let q = {src_val} ?? \"[]\"; \
                     let n = q == \"\" || q == \"null\" ? \"[]\" : q; \
                     len(fromJSON(s)) == len(fromJSON(n)) \
                     ? toJSON({{'Ok': {RENORM}}}) \
                     : {err}}}}}",
                    agg_value(task_scope, task),
                    err = argo_err("nil")
                ),
                dep(task),
            )
        }
    }
}

/// Same shape as [`arg_value`] but for an `Artifact<T>`-typed consumer
/// slot: the substitution string lives in `from:` of an
/// `arguments.artifacts[]` entry, referencing the producer's
/// `outputs.artifacts.return` (or `inputs.artifacts.<n>` for a
/// workflow-input forward). Argo wires artifacts by name, never via
/// expressions, so dotted-form is fine here.
///
/// Only `Arg::Ref` and `Arg::Input` are valid sources for an artifact
/// slot in v1 -- other Arg variants are blocked by the ghost via type
/// mismatch (e.g. `Arg::Lit` of a `String` literal can't satisfy an
/// `Artifact<String>` parameter). For belt-and-suspenders, those
/// variants fall back to the parameter-form string, which would be
/// rejected at submission if it ever escaped the ghost.
pub(crate) fn arg_value_artifact(arg: &Arg, steps: bool) -> (String, Option<String>) {
    let task_scope = if steps { "steps" } else { "tasks" };
    let dep = |t: &str| (!steps).then(|| t.to_string());
    match arg {
        Arg::Ref(t) => (
            format!("{{{{{task_scope}.{t}.outputs.artifacts.return}}}}"),
            dep(t),
        ),
        Arg::Input(n) => (format!("{{{{inputs.artifacts.{n}}}}}"), None),
        // Fallback to the parameter form; the ghost type-check
        // prevents these from ever reaching an artifact-typed slot in
        // valid programs.
        _ => arg_value(arg, steps),
    }
}

pub(crate) fn node_tokens(node: &Node, steps: bool) -> TokenStream2 {
    let task = &node.task;
    let callee = &node.callee;

    // Per-task arg: push the value template into `__params` (parameter
    // arg) or `__artifacts` (artifact arg) and, if the arg references
    // an upstream task in dag mode, also push the dep. Kind comes from
    // `<callee>::INPUT_KINDS[i]` at emit-time; backwards-compat default
    // (empty/short slice) is Parameter, so any callee that hasn't been
    // recompiled against the new trait keeps its old wiring.
    let arg_stmts = node.args.iter().enumerate().map(|(i, a)| {
        let (param_value, param_dep) = arg_value(a, steps);
        let (art_value, art_dep) = arg_value_artifact(a, steps);
        let param_dep_push = match param_dep {
            Some(d) => quote! { __deps.push(#d.to_string()); },
            None => quote! {},
        };
        let art_dep_push = match art_dep {
            Some(d) => quote! { __deps.push(#d.to_string()); },
            None => quote! {},
        };
        quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                let __kind = __kinds
                    .get(#i)
                    .copied()
                    .unwrap_or(::cargo_athena::IoKind::Parameter);
                match __kind {
                    ::cargo_athena::IoKind::Parameter => {
                        #param_dep_push
                        __params.push(::cargo_athena::api::Parameter {
                            name: __name,
                            value: ::core::option::Option::Some(#param_value.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
                    ::cargo_athena::IoKind::Artifact => {
                        #art_dep_push
                        __artifacts.push(::cargo_athena::api::Artifact {
                            name: __name,
                            from: #art_value.to_string(),
                            ..::core::default::Default::default()
                        });
                    }
                }
            }
        }
    });

    let push = if steps {
        quote! {
            // one sequential step group containing this single step
            __steps.push(::std::vec![__step]);
        }
    } else {
        quote! { __tasks.push(__step); }
    };

    // `.continue_on(...)` -> Argo continueOn.
    let continue_on_tok = match node.continue_on {
        Some((err, failed)) => quote! {
            ::core::option::Option::Some(::cargo_athena::api::ContinueOn {
                error: #err,
                failed: #failed,
            })
        },
        None => quote! { ::core::option::Option::None },
    };

    // `.on_exit(t)`/`.on_exit(t(args))` -> key `exit` (no expression);
    // `.on_success`/`.on_failure`/`.on_error`/`.hook_if(...)` ->
    // auto-keyed `hook1`,`hook2`,… in source order. Hook templates
    // resolve via the wormhole like callees; hook args reuse the same
    // `arg_value` lowering as task args (it has all the kinds covered),
    // but the dep half is dropped — hooks add no DAG dependency.
    let mut hook_n: u32 = 0;
    // Argo expression scope: a sibling node is `tasks['x']` in a dag,
    // `steps['x']` in a steps workflow. Bracket form (NOT `tasks.x`) so
    // hyphenated kebab task names resolve.
    let scope = if steps { "steps" } else { "tasks" };
    let hook_inserts: Vec<TokenStream2> = node
        .hooks
        .iter()
        .map(|h| {
            let hp = &h.template;
            let (key, expr_lit) = match &h.when {
                HookWhen::Exit => ("exit".to_string(), String::new()),
                when => {
                    hook_n += 1;
                    let expr = match when {
                        HookWhen::Success => format!("{scope}['{task}'].status == \"Succeeded\""),
                        HookWhen::Failure => format!("{scope}['{task}'].status == \"Failed\""),
                        HookWhen::Error => format!("{scope}['{task}'].status == \"Error\""),
                        HookWhen::Raw(s) => s.clone(),
                        HookWhen::Exit => unreachable!(),
                    };
                    (format!("hook{hook_n}"), expr)
                }
            };
            // Ghost-checked at compile time; belt-and-braces for a
            // path the ghost can't see: more args than declared INPUTS
            // would otherwise emit nameless parameters. `<` stays legal
            // — a trailing `#[inject]` tail widens INPUTS past the
            // caller-visible args.
            let n_args = h.args.len();
            let arity_guard = (n_args > 0).then(|| {
                quote! {
                    ::core::assert!(
                        #n_args <= __hin.len(),
                        "hook template `{}` declares {} input(s) but \
                         the hook call passes {} arg(s)",
                        __hn, __hin.len(), #n_args,
                    );
                }
            });
            let arg_pushes = h.args.iter().enumerate().map(|(i, a)| {
                // Hooks never push deps — same value template as task
                // args, just discard the dep half.
                let (value, _) = arg_value(a, steps);
                quote! {
                    __hp.push(::cargo_athena::api::Parameter {
                        name: __hin[#i].to_string(),
                        value: ::core::option::Option::Some(#value.to_string()),
                        ..::core::default::Default::default()
                    });
                }
            });
            quote! {
                {
                    let __hn =
                        <#hp as ::cargo_athena::Template>::ARGO_NAME;
                    let __hin: &[&str] =
                        <#hp as ::cargo_athena::Template>::INPUTS;
                    #arity_guard
                    let mut __hp: ::std::vec::Vec<
                        ::cargo_athena::api::Parameter,
                    > = ::std::vec::Vec::new();
                    #( #arg_pushes )*
                    let __hargs = if __hp.is_empty() {
                        ::core::option::Option::None
                    } else {
                        ::core::option::Option::Some(
                            ::cargo_athena::api::Arguments {
                                parameters: __hp,
                                ..::core::default::Default::default()
                            },
                        )
                    };
                    __hooks.insert(
                        #key.to_string(),
                        ::cargo_athena::api::LifecycleHook {
                            template_ref: ::core::option::Option::Some(
                                ::cargo_athena::api::TemplateRef {
                                    name: __hn.to_string(),
                                    template: __hn.to_string(),
                                    cluster_scope: false,
                                },
                            ),
                            arguments: __hargs,
                            expression: #expr_lit.to_string(),
                        },
                    );
                }
            }
        })
        .collect();

    // `fan_out` -> Argo `withParam` over the list source (+ a DAG dep on
    // the producing task when the source is a prior binding). Dotted
    // plain-tag form: the source can never be a `.continue_on` binding
    // (the ghost rejects fanning a `Result`), so it resolves or the fan
    // is legitimately omitted with its failed producer.
    let (with_param_val, fan_dep) = match &node.fan {
        Some(FanSrc::Task(dep)) => (
            format!("{{{{{scope}.{dep}.outputs.parameters.return}}}}"),
            if steps {
                quote! {}
            } else {
                quote! { __deps.push(#dep.to_string()); }
            },
        ),
        Some(FanSrc::Input(name)) => (format!("{{{{inputs.parameters.{name}}}}}"), quote! {}),
        None => (String::new(), quote! {}),
    };

    // `if`-wrapper arm tasks carry a fully-rendered Argo `when`.
    let when_val = match &node.when {
        Some(w) => quote! { #w.to_string() },
        None => quote! { ::std::string::String::new() },
    };

    quote! {
        {
            // Resolved by the type system across modules/crates:
            let __ref_name =
                <#callee as ::cargo_athena::Template>::ARGO_NAME;
            let __inputs: &[&str] =
                <#callee as ::cargo_athena::Template>::INPUTS;
            // Per-input I/O kind: drives `arguments.parameters` vs
            // `arguments.artifacts.from` dispatch per arg slot. Empty
            // slice (the backwards-compat default on a callee that
            // predates `Artifact<T>`) means "all parameters", same as
            // today.
            let __kinds: &[::cargo_athena::IoKind] =
                <#callee as ::cargo_athena::Template>::INPUT_KINDS;
            let mut __params: ::std::vec::Vec<::cargo_athena::api::Parameter> =
                ::std::vec::Vec::new();
            let mut __artifacts: ::std::vec::Vec<::cargo_athena::api::Artifact> =
                ::std::vec::Vec::new();
            let mut __deps: ::std::vec::Vec<::std::string::String> =
                ::std::vec::Vec::new();
            #fan_dep
            #( #arg_stmts )*
            __deps.sort();
            __deps.dedup();
            let __continue_on = #continue_on_tok;
            let mut __hooks: ::std::collections::BTreeMap<
                ::std::string::String,
                ::cargo_athena::api::LifecycleHook,
            > = ::std::collections::BTreeMap::new();
            #( #hook_inserts )*
            let __step = ::cargo_athena::api::DagTask {
                name: #task.to_string(),
                template: ::std::string::String::new(),
                dependencies: __deps,
                arguments: ::core::option::Option::Some(::cargo_athena::api::Arguments {
                    parameters: __params,
                    artifacts: __artifacts,
                }),
                template_ref: ::core::option::Option::Some(
                    ::cargo_athena::api::TemplateRef {
                        name: __ref_name.to_string(),
                        template: __ref_name.to_string(),
                        cluster_scope: false,
                    }
                ),
                continue_on: __continue_on,
                hooks: __hooks,
                with_param: #with_param_val.to_string(),
                when: #when_val,
            };
            #push
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::analyze::JsonSrc;

    #[test]
    fn lit_is_passthrough() {
        assert_eq!(
            arg_value(&Arg::Lit("\"v\"".to_string()), false),
            ("\"v\"".to_string(), None),
        );
        // Steps mode is identical for Lit.
        assert_eq!(
            arg_value(&Arg::Lit("7".to_string()), true),
            ("7".to_string(), None),
        );
    }

    #[test]
    fn ref_dag_emits_dep_dotted_form() {
        let (v, d) = arg_value(&Arg::Ref("ingest".to_string()), false);
        assert_eq!(v, "{{tasks.ingest.outputs.parameters.return}}");
        assert_eq!(d, Some("ingest".to_string()));
    }

    #[test]
    fn ref_steps_drops_dep_uses_steps_scope() {
        let (v, d) = arg_value(&Arg::Ref("ingest".to_string()), true);
        assert_eq!(v, "{{steps.ingest.outputs.parameters.return}}");
        assert_eq!(d, None);
    }

    #[test]
    fn input_no_dep() {
        let (v, d) = arg_value(&Arg::Input("x".to_string()), false);
        assert_eq!(v, "{{inputs.parameters.x}}");
        assert_eq!(d, None);
    }

    #[test]
    fn json_task_uses_bracket_form_and_tojson_fromjson() {
        let (v, d) = arg_value(
            &Arg::Json {
                src: JsonSrc::Task("dep".to_string()),
                path: vec!["a".to_string(), "b".to_string()],
            },
            false,
        );
        assert_eq!(
            v,
            "{{=toJSON(fromJSON(tasks['dep'].outputs.parameters['return'])['a']['b'])}}"
        );
        assert_eq!(d, Some("dep".to_string()));
    }

    #[test]
    fn json_input_no_dep() {
        let (v, d) = arg_value(
            &Arg::Json {
                src: JsonSrc::Input("meta".to_string()),
                path: vec!["id".to_string()],
            },
            false,
        );
        assert_eq!(v, "{{=toJSON(fromJSON(inputs.parameters['meta'])['id'])}}");
        assert_eq!(d, None);
    }

    #[test]
    fn item_bare() {
        let (v, d) = arg_value(&Arg::Item { path: vec![] }, false);
        assert_eq!(v, "{{item}}");
        assert_eq!(d, None);
    }

    #[test]
    fn item_with_field_chain() {
        let (v, d) = arg_value(
            &Arg::Item {
                path: vec!["f".to_string(), "g".to_string()],
            },
            false,
        );
        assert_eq!(v, "{{item.f.g}}");
        assert_eq!(d, None);
    }

    #[test]
    fn fanagg_emits_kind_aware_renorm() {
        let (v, d) = arg_value(&Arg::FanAgg("caps".to_string()), false);
        assert_eq!(
            v,
            "{{=let g = tasks?.['caps']; \
             let p = g?.outputs?.parameters; \
             let r = (type(p) == \"map\" ? p?.['return'] : nil) ?? \"[]\"; \
             let s = r == \"\" || r == \"null\" ? \"[]\" : r; \
             toJSON(map(fromJSON(s), { type(#) == \"string\" ? fromJSON(#) : # }))}}"
        );
        assert_eq!(d, Some("caps".to_string()));
    }

    #[test]
    fn fanagg_steps_no_dep() {
        let (_, d) = arg_value(&Arg::FanAgg("caps".to_string()), true);
        assert_eq!(d, None);
    }

    #[test]
    fn ref_fallible_encodes_result() {
        let (v, d) = arg_value(&Arg::RefFallible("risky".to_string()), false);
        assert_eq!(
            v,
            "{{=let g = tasks?.['risky']; \
             let c = g?.outputs?.exitCode; \
             g?.status == \"Succeeded\" \
             ? toJSON({'Ok': fromJSON(g?.outputs?.parameters?.['return'] ?? \"null\")}) \
             : toJSON({'Err': {'status': g?.status ?? \"Failed\", \
             'exit_code': c == nil ? nil : int(c)} })}}"
        );
        assert_eq!(d, Some("risky".to_string()));
    }

    #[test]
    fn fanagg_fallible_is_all_or_nothing() {
        let (v, d) = arg_value(
            &Arg::FanAggFallible {
                task: "caps".to_string(),
                src: FanSrc::Task("mk".to_string()),
            },
            false,
        );
        assert_eq!(
            v,
            "{{=let g = tasks?.['caps']; \
             let p = g?.outputs?.parameters; \
             let r = (type(p) == \"map\" ? p?.['return'] : nil) ?? \"[]\"; \
             let s = r == \"\" || r == \"null\" ? \"[]\" : r; \
             let q = tasks?.['mk']?.outputs?.parameters?.['return'] ?? \"[]\"; \
             let n = q == \"\" || q == \"null\" ? \"[]\" : q; \
             len(fromJSON(s)) == len(fromJSON(n)) \
             ? toJSON({'Ok': map(fromJSON(s), { type(#) == \"string\" ? fromJSON(#) : # })}) \
             : toJSON({'Err': {'status': g?.status ?? \"Failed\", 'exit_code': nil} })}}"
        );
        assert_eq!(d, Some("caps".to_string()));
    }

    #[test]
    fn fanagg_fallible_input_src() {
        let (v, _) = arg_value(
            &Arg::FanAggFallible {
                task: "caps".to_string(),
                src: FanSrc::Input("items".to_string()),
            },
            false,
        );
        assert!(v.contains("let q = inputs?.parameters?.['items'] ?? \"[]\""));
    }
}
