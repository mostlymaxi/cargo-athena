//! Lower one [`Node`] to the `quote!` block that pushes a `DagTask` (or
//! `Vec<DagTask>` step group) into the workflow's `Template::build`.
//!
//! This is the only producer of the Argo `templateRef` + parameter +
//! dependency wiring. Called once per task by [`crate::workflow::expand`]
//! and once per arm-task by [`crate::conditional::emit_synth`].

use proc_macro2::TokenStream as TokenStream2;
use quote::quote;

use crate::analyze::{Arg, FanSrc, HookWhen, JsonSrc, Node};

pub(crate) fn node_tokens(node: &Node, steps: bool) -> TokenStream2 {
    let task = &node.task;
    let callee = &node.callee;
    let ref_scope = if steps { "{{steps." } else { "{{tasks." };

    let arg_stmts = node.args.iter().enumerate().map(|(i, a)| match a {
        Arg::Lit(v) => quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(#v.to_string()),
                    ..::core::default::Default::default()
                });
            }
        },
        Arg::Ref(dep) => {
            let dep_push = if steps {
                quote! {}
            } else {
                quote! { __deps.push(#dep.to_string()); }
            };
            quote! {
                {
                    let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                    #dep_push
                    let mut __v = ::std::string::String::from(#ref_scope);
                    __v.push_str(#dep);
                    // `outputs.parameters.return` — the explicitly declared
                    // param. NOT `outputs.result` (Argo's script-stdout
                    // alias: only exists for container/script tmpls, never
                    // dag/steps, so a sub-workflow's return needs this).
                    __v.push_str(".outputs.parameters.return}}");
                    __params.push(::cargo_athena::api::Parameter {
                        name: __name,
                        value: ::core::option::Option::Some(__v),
                        ..::core::default::Default::default()
                    });
                }
            }
        }
        Arg::Input(name) => quote! {
            {
                let __name = __inputs.get(#i).copied().unwrap_or_default().to_string();
                let mut __v = ::std::string::String::from("{{inputs.parameters.");
                __v.push_str(#name);
                __v.push_str("}}");
                __params.push(::cargo_athena::api::Parameter {
                    name: __name,
                    value: ::core::option::Option::Some(__v),
                    ..::core::default::Default::default()
                });
            }
        },
        // `a.b.c` -> Argo expr-templating. toJSON(fromJSON(..)) is the
        // universal-safe round-trip (athena's run-side is `from_str` else
        // String, so it reconstructs every field type incl. quoted
        // strings & nested structs). Bracket form is hyphen/keyword-safe.
        Arg::Json { src, path } => {
            let scope = if steps { "steps" } else { "tasks" };
            let accessor: String = path.iter().map(|f| format!("['{f}']")).collect();
            let (refexpr, dep_push) = match src {
                JsonSrc::Task(dep) => {
                    let r = format!("{scope}['{dep}'].outputs.parameters['return']");
                    let dp = if steps {
                        quote! {}
                    } else {
                        quote! { __deps.push(#dep.to_string()); }
                    };
                    (r, dp)
                }
                JsonSrc::Input(name) => (format!("inputs.parameters['{name}']"), quote! {}),
            };
            let value = format!("{{{{=toJSON(fromJSON({refexpr}){accessor})}}}}");
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
        }
        // The fan-out closure param: `{{item}}` / `{{item.f.g}}`. Argo
        // binds `item` per iteration of this task's `withParam`.
        Arg::Item { path } => {
            let mut v = String::from("{{item");
            for f in path {
                v.push('.');
                v.push_str(f);
            }
            v.push_str("}}");
            quote! {
                __params.push(::cargo_athena::api::Parameter {
                    name: __inputs.get(#i).copied().unwrap_or_default().to_string(),
                    value: ::core::option::Option::Some(#v.to_string()),
                    ..::core::default::Default::default()
                });
            }
        }
        // Consuming a `fan_out` aggregate. Argo's `aggregatedJSONValueList`
        // (controller/operator.go) is **type-heterogeneous** — proven
        // from Argo v4.0.5 source: `tryJSONUnmarshal` keeps elements
        // that parse to an object/array as native JSON, but for every
        // JSON *scalar* (string/number/bool/null) hits `default:
        // success=false` and falls back to `json.Marshal([]string{…})`
        // (raw, escaped). So the aggregate is EITHER `[{…},…]` (native)
        // OR `["\"v\"",…]` (escaped scalars). One universal kind-aware
        // re-normalization keyed off the actual aggregate-element kind
        // (NOT the Rust type — unknowable cross-crate): per element,
        // `fromJSON` it iff it came back a string (the stringified-
        // scalar case), else pass the parsed object/array through;
        // then re-`toJSON` the array. (`type`/`map`/`fromJSON`/`toJSON`
        // are expr-lang v1.17 builtins.)
        Arg::FanAgg(dep) => {
            let scope = if steps { "steps" } else { "tasks" };
            let dep_push = if steps {
                quote! {}
            } else {
                quote! { __deps.push(#dep.to_string()); }
            };
            let value = format!(
                "{{{{=toJSON(map(fromJSON({scope}['{dep}']\
                 .outputs.parameters['return']), \
                 {{ type(#) == \"string\" ? fromJSON(#) : # }}))}}}}"
            );
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
    // `.hooks("e" = t, …)` -> keys hook1, hook2, … (source order). Hook
    // templates resolve their Argo name + INPUTS via the wormhole, like
    // callees; hook args use the same scope as the task's own args
    // (literal / workflow input / prior binding), but add NO dependency.
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
            let arg_pushes = h.args.iter().enumerate().map(|(i, a)| match a {
                Arg::Lit(v) => quote! {
                    __hp.push(::cargo_athena::api::Parameter {
                        name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                        value: ::core::option::Option::Some(#v.to_string()),
                        ..::core::default::Default::default()
                    });
                },
                Arg::Ref(dep) => quote! {
                    {
                        let mut __v = ::std::string::String::from(#ref_scope);
                        __v.push_str(#dep);
                        __v.push_str(".outputs.parameters.return}}");
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(__v),
                            ..::core::default::Default::default()
                        });
                    }
                },
                Arg::Input(name) => quote! {
                    {
                        let mut __v =
                            ::std::string::String::from("{{inputs.parameters.");
                        __v.push_str(#name);
                        __v.push_str("}}");
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(__v),
                            ..::core::default::Default::default()
                        });
                    }
                },
                // Same expr-templating lowering as task args; hooks add
                // no DAG dependency.
                Arg::Json { src, path } => {
                    let s = if steps { "steps" } else { "tasks" };
                    let accessor: String = path.iter().map(|f| format!("['{f}']")).collect();
                    let refexpr = match src {
                        JsonSrc::Task(dep) => format!("{s}['{dep}'].outputs.parameters['return']"),
                        JsonSrc::Input(name) => {
                            format!("inputs.parameters['{name}']")
                        }
                    };
                    let value = format!("{{{{=toJSON(fromJSON({refexpr}){accessor})}}}}");
                    quote! {
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(#value.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
                }
                Arg::Item { path } => {
                    let mut v = String::from("{{item");
                    for f in path {
                        v.push('.');
                        v.push_str(f);
                    }
                    v.push_str("}}");
                    quote! {
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(#v.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
                }
                // Same array-renormalizing form as task args; hooks add
                // no DAG dependency.
                Arg::FanAgg(dep) => {
                    let s = if steps { "steps" } else { "tasks" };
                    let value = format!(
                        "{{{{=toJSON(map(fromJSON({s}['{dep}']\
                         .outputs.parameters['return']), \
                         {{ type(#) == \"string\" ? fromJSON(#) : # }}))}}}}"
                    );
                    quote! {
                        __hp.push(::cargo_athena::api::Parameter {
                            name: __hin.get(#i).copied().unwrap_or_default().to_string(),
                            value: ::core::option::Option::Some(#value.to_string()),
                            ..::core::default::Default::default()
                        });
                    }
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
    // the producing task when the source is a prior binding).
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
