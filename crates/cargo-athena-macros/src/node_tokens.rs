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
        // Consuming a `fan_out` aggregate. Argo's `aggregatedJSONValueList`
        // (controller/operator.go) is **type-heterogeneous** — proven
        // from v4.0.5 source: `tryJSONUnmarshal` keeps elements parsing
        // to an object/array as native JSON, but every JSON scalar
        // (string/number/bool/null) hits `default: success=false` and
        // falls back to `json.Marshal([]string{…})` (raw, escaped). So
        // the aggregate is EITHER `[{…},…]` (native) OR `["\"v\"",…]`
        // (escaped scalars). One universal kind-aware re-norm keyed off
        // the actual element kind (NOT the Rust type — unknowable
        // cross-crate): per element, `fromJSON` it iff it came back a
        // string, else pass the parsed object/array through; then
        // re-`toJSON` the array. (`type`/`map`/`fromJSON`/`toJSON` are
        // expr-lang v1.17 builtins.)
        Arg::FanAgg(t) => (
            format!(
                "{{{{=toJSON(map(fromJSON({task_scope}['{t}']\
                 .outputs.parameters['return']), \
                 {{ type(#) == \"string\" ? fromJSON(#) : # }}))}}}}"
            ),
            dep(t),
        ),
    }
}

pub(crate) fn node_tokens(node: &Node, steps: bool) -> TokenStream2 {
    let task = &node.task;
    let callee = &node.callee;

    // Per-task arg: push the value template into `__params` and, if the
    // arg references an upstream task in dag mode, also push the dep.
    let arg_stmts = node.args.iter().enumerate().map(|(i, a)| {
        let (value, dep) = arg_value(a, steps);
        let dep_push = match dep {
            Some(d) => quote! { __deps.push(#d.to_string()); },
            None => quote! {},
        };
        quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                #dep_push
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(#value.to_string()),
                    ..::core::default::Default::default()
                });
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
            let arg_pushes = h.args.iter().enumerate().map(|(i, a)| {
                // Hooks never push deps — same value template as task
                // args, just discard the dep half.
                let (value, _) = arg_value(a, steps);
                quote! {
                    __hp.push(::cargo_athena::api::Parameter {
                        name: __hin.get(#i).copied().unwrap_or_default().to_string(),
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
    // the producing task when the source is a prior binding). Uses the
    // dotted form (not `arg_value`'s bracket form) because `withParam`
    // is plain Argo templating, not an expr-template.
    let ref_scope = if steps { "{{steps." } else { "{{tasks." };
    let (with_param_val, fan_dep) = match &node.fan {
        Some(FanSrc::Task(dep)) => (
            format!("{ref_scope}{dep}.outputs.parameters.return}}}}"),
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
            let mut __params: ::std::vec::Vec<::cargo_athena::api::Parameter> =
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
                    ..::core::default::Default::default()
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
            "{{=toJSON(map(fromJSON(tasks['caps'].outputs.parameters['return']), \
             { type(#) == \"string\" ? fromJSON(#) : # }))}}"
        );
        assert_eq!(d, Some("caps".to_string()));
    }

    #[test]
    fn fanagg_steps_no_dep() {
        let (_, d) = arg_value(&Arg::FanAgg("caps".to_string()), true);
        assert_eq!(d, None);
    }
}
