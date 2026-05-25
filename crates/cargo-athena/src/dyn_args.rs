//! Dynamic per-template clap CLI for `cargo athena submit`.
//!
//! At submit time we know each template's parameter names and the
//! stringified Rust types of those parameters (via `ContainerRunMeta`).
//! This module turns that into a tailored `clap::Command` so the user
//! can write `--url <STR> --depth <N>` instead of `-a url=… -a depth=…`,
//! with `--help` listing the typed arguments per template.
//!
//! Scalar types (`String`, integers, floats, `bool`, `Vec<scalar>`,
//! `Option<scalar>`) map to typed clap flags. Non-scalar types (user
//! structs, maps, anything we don't recognize from the type string)
//! get a `--<name> <JSON>` flag plus a sibling `--<name>-from-file
//! <PATH>`, mutually exclusive. The classic `-a name=value` is kept as
//! an escape hatch.

use cargo_athena::serde_json;
use clap::{Arg, ArgAction, Command};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

/// clap 4's `Arg::new` / `.long` want `impl IntoResettable<{Id,Str}>`,
/// which is implemented for `&'static str` but not for owned `String`.
/// We build the Command once per `submit` invocation, so leaking each
/// dynamic string for the process lifetime is cheap and avoids
/// fighting the type system with `Box<str>` ceremony everywhere.
fn leak(s: String) -> &'static str {
    Box::leak(s.into_boxed_str())
}

/// Coarse Rust-type bucket inferred from the stringified type the
/// macro records in `INPUT_TYPES`.
#[derive(Clone, Copy)]
pub enum Kind {
    String,
    Integer,
    Float,
    Bool,
    /// `Vec<T>` of a scalar T - repeatable typed flag.
    VecOf(&'static ScalarSpec),
    /// Anything not in the table above (user structs, maps, etc.).
    /// Exposed as `--name <JSON>` + `--name-from-file <PATH>`.
    Json,
}

/// One scalar entry: how clap should label it in help, and how to
/// validate / re-encode the raw string into a JSON value.
pub struct ScalarSpec {
    pub label: &'static str,
    encode: fn(&str) -> Result<serde_json::Value, String>,
}

fn enc_str(s: &str) -> Result<serde_json::Value, String> {
    Ok(serde_json::Value::String(s.to_string()))
}
fn enc_int(s: &str) -> Result<serde_json::Value, String> {
    s.parse::<i64>()
        .map(|n| serde_json::Value::Number(n.into()))
        .map_err(|e| format!("not a valid integer: {e}"))
}
fn enc_uint(s: &str) -> Result<serde_json::Value, String> {
    s.parse::<u64>()
        .map(|n| serde_json::Value::Number(n.into()))
        .map_err(|e| format!("not a valid unsigned integer: {e}"))
}
fn enc_float(s: &str) -> Result<serde_json::Value, String> {
    let f: f64 = s.parse().map_err(|e| format!("not a valid number: {e}"))?;
    serde_json::Number::from_f64(f)
        .map(serde_json::Value::Number)
        .ok_or_else(|| format!("{f} is not representable in JSON (NaN / infinity)"))
}
fn enc_bool(s: &str) -> Result<serde_json::Value, String> {
    s.parse::<bool>()
        .map(serde_json::Value::Bool)
        .map_err(|e| format!("not a valid bool (use `true` or `false`): {e}"))
}

const STRING: ScalarSpec = ScalarSpec {
    label: "STRING",
    encode: enc_str,
};
const INT: ScalarSpec = ScalarSpec {
    label: "INT",
    encode: enc_int,
};
const UINT: ScalarSpec = ScalarSpec {
    label: "UINT",
    encode: enc_uint,
};
const FLOAT: ScalarSpec = ScalarSpec {
    label: "FLOAT",
    encode: enc_float,
};
const BOOL: ScalarSpec = ScalarSpec {
    label: "true|false",
    encode: enc_bool,
};

fn scalar_for(ty: &str) -> Option<&'static ScalarSpec> {
    let ty = ty.trim();
    match ty {
        "String" | "&str" | "&'static str" => Some(&STRING),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" => Some(&INT),
        "u8" | "u16" | "u32" | "u64" | "u128" | "usize" => Some(&UINT),
        "f32" | "f64" => Some(&FLOAT),
        "bool" => Some(&BOOL),
        _ => None,
    }
}

/// Strip one layer of `Wrapper<…>` if `ty` matches. Used to peel
/// `Option<T>` and `Vec<T>` to look at the inner type.
fn unwrap1<'a>(ty: &'a str, wrapper: &str) -> Option<&'a str> {
    let ty = ty.trim();
    let prefix = format!("{wrapper}<");
    let inner = ty.strip_prefix(&prefix)?.strip_suffix('>')?.trim();
    Some(inner)
}

/// Classify a stringified Rust type.
pub fn classify(ty: &str) -> Kind {
    if let Some(s) = scalar_for(ty) {
        return match s.label {
            "STRING" => Kind::String,
            "INT" | "UINT" => Kind::Integer,
            "FLOAT" => Kind::Float,
            "true|false" => Kind::Bool,
            _ => Kind::String,
        };
    }
    if let Some(inner) = unwrap1(ty, "Option")
        && let Some(_) = scalar_for(inner)
    {
        // Option<scalar> behaves the same as the scalar at the CLI;
        // clap's "not required" already covers the None case.
        return classify(inner);
    }
    if let Some(inner) = unwrap1(ty, "Vec")
        && let Some(spec) = scalar_for(inner)
    {
        // Vec<T> for scalar T => repeatable typed flag.
        return Kind::VecOf(static_scalar(spec));
    }
    Kind::Json
}

fn static_scalar(s: &ScalarSpec) -> &'static ScalarSpec {
    match s.label {
        "STRING" => &STRING,
        "INT" => &INT,
        "UINT" => &UINT,
        "FLOAT" => &FLOAT,
        "true|false" => &BOOL,
        _ => &STRING,
    }
}

/// Convert a snake_case (or anything-else) ident to a kebab-case CLI
/// flag name. `top_n` -> `top-n`, `topN` -> `top-n`.
pub fn kebab(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 2);
    let mut prev_lower = false;
    for c in name.chars() {
        if c == '_' {
            out.push('-');
            prev_lower = false;
        } else if c.is_uppercase() {
            if prev_lower {
                out.push('-');
            }
            for low in c.to_lowercase() {
                out.push(low);
            }
            prev_lower = false;
        } else {
            out.push(c);
            prev_lower = c.is_lowercase() || c.is_ascii_digit();
        }
    }
    out
}

/// One typed argument added to the dynamic Command, with all the
/// metadata we need afterwards to extract its value.
struct DynArg {
    /// Param name as the Rust function arg sees it (also the JSON key).
    name: String,
    /// Long flag (kebab name).
    flag: String,
    /// `--<flag>-from-file` (only populated for non-scalar types).
    from_file: Option<String>,
    /// Stringified Rust type for help text and error messages.
    rust_type: String,
    kind: Kind,
}

/// Build a `clap::Command` that knows the template's typed args plus
/// the submit-specific static flags. Used after we've fetched the
/// template's metadata.
pub fn build_command(template: &str, params: &[(String, String)]) -> (Command, Vec<DynArgIdent>) {
    let mut cmd = Command::new(leak(format!("cargo athena submit {template}")))
        .no_binary_name(true)
        .disable_help_flag(false)
        .about(leak(format!(
            "Submit `{template}` to a real cluster. Function arguments \
             appear below as typed `--<name>` flags; use `-a name=value` \
             for any argument typed as a struct / map / other non-scalar."
        )));

    let mut idents: Vec<DynArgIdent> = Vec::new();

    // --- one --<name> per template parameter ------------------------------
    for (raw_name, raw_ty) in params {
        let flag = kebab(raw_name);
        let kind = classify(raw_ty);
        // Not `.required(true)` — the user might supply this via
        // `-a name=value` or `--input-file` instead. `validate_args`
        // afterwards reports any missing args with one CLI-style report.
        let mut arg = Arg::new(leak(raw_name.clone()))
            .long(leak(flag.clone()))
            .help(leak(format!("template arg `{raw_name}: {raw_ty}`")));
        match kind {
            Kind::String => arg = arg.value_name("STRING"),
            Kind::Integer => arg = arg.value_name("INT"),
            Kind::Float => arg = arg.value_name("FLOAT"),
            Kind::Bool => arg = arg.value_name("true|false"),
            Kind::VecOf(spec) => {
                arg = arg
                    .value_name(spec.label)
                    .action(ArgAction::Append)
                    .help(leak(format!(
                        "template arg `{raw_name}: {raw_ty}` (repeat for each element)"
                    )));
            }
            Kind::Json => {
                arg = arg.value_name("JSON").help(leak(format!(
                    "template arg `{raw_name}: {raw_ty}` (non-scalar type \
                     - pass JSON, e.g. '{{\"k\":\"v\"}}'). For large or \
                     multi-line values, use --{flag}-from-file instead."
                )));
            }
        }
        cmd = cmd.arg(arg);

        let from_file = if matches!(kind, Kind::Json) {
            let ff_flag = format!("{flag}-from-file");
            let ff_id = format!("__{raw_name}_from_file");
            let raw_name_static: &'static str = leak(raw_name.clone());
            cmd = cmd.arg(
                Arg::new(leak(ff_id.clone()))
                    .long(leak(ff_flag.clone()))
                    .value_name("PATH")
                    .help(leak(format!(
                        "read template arg `{raw_name}` as JSON from this file"
                    )))
                    .conflicts_with(raw_name_static),
            );
            Some(ff_id)
        } else {
            None
        };

        idents.push(DynArgIdent {
            inner: DynArg {
                name: raw_name.clone(),
                flag,
                from_file,
                rust_type: raw_ty.clone(),
                kind,
            },
        });
    }

    // --- shared escape hatch + submit-specific flags ----------------------
    cmd = cmd
        .arg(
            Arg::new("a_arg")
                .short('a')
                .long("arg")
                .value_name("NAME=VALUE")
                .action(ArgAction::Append)
                .help(
                    "Set one argument as `name=value` (JSON-else-string). \
                     Repeatable. Use for struct-typed args or to override.",
                ),
        )
        .arg(
            Arg::new("input_file")
                .long("input-file")
                .value_name("FILE")
                .help("JSON object merged under `-a` (so per-arg flags / -a override it)"),
        )
        .arg(
            Arg::new("package")
                .short('p')
                .long("package")
                .value_name("PKG")
                .help("Cargo package to drive (else [defaults].package, else autodetect)"),
        )
        .arg(
            Arg::new("bin")
                .long("bin")
                .value_name("BIN")
                .help("Cargo bin within the package (else [defaults].bin, else autodetect)"),
        )
        .arg(
            Arg::new("namespace")
                .short('n')
                .long("namespace")
                .value_name("NS")
                .help("K8s namespace ($ARGO_NAMESPACE -> [defaults].namespace -> default)"),
        )
        .arg(
            Arg::new("service_account")
                .long("service-account")
                .value_name("SA")
                .help("ServiceAccount for the run"),
        )
        .arg(
            Arg::new("node_selector")
                .long("node-selector")
                .value_name("K=V")
                .action(ArgAction::Append)
                .help("Root-scoped nodeSelector (k=v, repeatable)"),
        )
        .arg(
            Arg::new("priority")
                .long("priority")
                .value_name("N")
                .value_parser(clap::value_parser!(i32))
                .help("Workflow priority (int32; higher = scheduled first)"),
        )
        .arg(
            Arg::new("argo_server")
                .long("argo-server")
                .value_name("URL")
                .help("Argo Server REST URL (else kube API)"),
        )
        .arg(
            Arg::new("insecure")
                .long("insecure-skip-tls-verify")
                .action(ArgAction::SetTrue)
                .help("Skip TLS verification on the Argo Server connection"),
        )
        .arg(
            Arg::new("update")
                .long("update")
                .action(ArgAction::SetTrue)
                .help("Re-apply every WorkflowTemplate even if unchanged"),
        )
        .arg(
            Arg::new("skip_binary_check")
                .long("skip-binary-check")
                .action(ArgAction::SetTrue)
                .help("Skip the pre-flight S3 HEAD on the binary tarball"),
        )
        .arg(
            Arg::new("yes")
                .short('y')
                .long("yes")
                .action(ArgAction::SetTrue)
                .help("Assume yes for every prompt"),
        );

    (cmd, idents)
}

/// Opaque handle the caller uses to extract one argument's value
/// after `clap::Command::try_get_matches_from`.
pub struct DynArgIdent {
    inner: DynArg,
}

/// Resolve the user's typed flags + `-a` / `--input-file` into a
/// flat `{name: JSON}` map. Detects conflicts where both a `--<flag>`
/// and a matching `-a name=value` are given for the same parameter
/// (refuses; the user has to pick one).
pub fn extract(
    matches: &clap::ArgMatches,
    idents: &[DynArgIdent],
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    use serde_json::Value;
    let mut out: BTreeMap<String, Value> = BTreeMap::new();

    // 1. `--input-file` (lowest priority).
    if let Some(path) = matches.get_one::<String>("input_file") {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("--input-file {path}: {e}"))?;
        match serde_json::from_str::<Value>(&text) {
            Ok(Value::Object(m)) => out.extend(m),
            _ => return Err("--input-file must be a JSON object".into()),
        }
    }

    // 2. `-a name=value` (middle priority). Track which names appeared
    //    so we can flag conflicts with the typed `--<flag>` form below.
    let mut a_names: BTreeSet<String> = BTreeSet::new();
    if let Some(pairs) = matches.get_many::<String>("a_arg") {
        for pair in pairs {
            let (k, v) = pair
                .split_once('=')
                .ok_or_else(|| format!("-a expects name=value, got {pair:?}"))?;
            let val = serde_json::from_str::<Value>(v).unwrap_or(Value::String(v.to_string()));
            a_names.insert(k.to_string());
            out.insert(k.to_string(), val);
        }
    }

    // 3. Typed `--<flag>` (highest priority). Ambiguity guard: the
    //    user can't set the same parameter twice across forms.
    for ident in idents {
        let a = &ident.inner;
        let typed_present = matches.contains_id(a.name.as_str())
            || a.from_file
                .as_ref()
                .is_some_and(|f| matches.contains_id(f.as_str()));
        if typed_present && a_names.contains(&a.name) {
            return Err(format!(
                "argument `{name}` was set by both `--{flag}` (or `--{flag}-from-file`) \
                 and `-a {name}=…`. Pick one.",
                name = a.name,
                flag = a.flag,
            ));
        }

        // --<flag>-from-file beats --<flag> (clap's `conflicts_with` already
        // rules out both being set at once; this just extracts whichever).
        if let Some(ff) = a.from_file.as_ref()
            && let Some(path) = matches.get_one::<String>(ff)
        {
            let text = std::fs::read_to_string(path)
                .map_err(|e| format!("--{}-from-file {}: {e}", a.flag, path))?;
            let val = serde_json::from_str::<Value>(text.trim())
                .map_err(|e| format!("--{}-from-file {}: not valid JSON: {e}", a.flag, path))?;
            out.insert(a.name.clone(), val);
            continue;
        }

        match a.kind {
            Kind::VecOf(spec) => {
                if let Some(raw) = matches.get_many::<String>(a.name.as_str()) {
                    let mut items: Vec<Value> = Vec::new();
                    for r in raw {
                        items.push(
                            (spec.encode)(r)
                                .map_err(|e| format!("--{} ({}): {e}", a.flag, a.rust_type))?,
                        );
                    }
                    out.insert(a.name.clone(), Value::Array(items));
                }
            }
            Kind::Json => {
                if let Some(raw) = matches.get_one::<String>(a.name.as_str()) {
                    let val = serde_json::from_str::<Value>(raw).map_err(|e| {
                        format!(
                            "--{} ({}): expected JSON, got {raw:?}: {e}",
                            a.flag, a.rust_type
                        )
                    })?;
                    out.insert(a.name.clone(), val);
                }
            }
            _ => {
                if let Some(raw) = matches.get_one::<String>(a.name.as_str()) {
                    // The scalar lookup is on the original raw type
                    // (Option<T> is collapsed to T by classify, so we
                    // re-peek the type here to pick the encoder).
                    let inner_ty = unwrap1(&a.rust_type, "Option").unwrap_or(&a.rust_type);
                    let spec = scalar_for(inner_ty).unwrap_or(&STRING);
                    let v = (spec.encode)(raw)
                        .map_err(|e| format!("--{} ({}): {e}", a.flag, a.rust_type))?;
                    out.insert(a.name.clone(), v);
                }
            }
        }
    }

    Ok(out)
}

/// Phase-1 parser: scan the trailing-arg vec for `--package` / `-p`
/// and `--bin` so we know which workflow binary to invoke before we
/// can build the phase-2 dynamic Command. A plain scan (not clap)
/// because clap's `ignore_errors(true)` short-circuits at the first
/// unknown flag, which loses values after a `--help` or any typed
/// per-template flag that appears earlier in the argv.
pub fn extract_pkg(rest: &[OsString]) -> (Option<String>, Option<String>) {
    let mut pkg = None;
    let mut bin = None;
    let mut iter = rest.iter();
    while let Some(arg) = iter.next() {
        let a = arg.to_string_lossy();
        let take_next = |it: &mut std::slice::Iter<'_, OsString>| {
            it.next().map(|s| s.to_string_lossy().into_owned())
        };
        match a.as_ref() {
            "-p" | "--package" => pkg = take_next(&mut iter),
            "--bin" => bin = take_next(&mut iter),
            s if s.starts_with("--package=") => {
                pkg = s.strip_prefix("--package=").map(str::to_string);
            }
            s if s.starts_with("--bin=") => {
                bin = s.strip_prefix("--bin=").map(str::to_string);
            }
            s if s.starts_with("-p=") => {
                pkg = s.strip_prefix("-p=").map(str::to_string);
            }
            _ => {}
        }
    }
    (pkg, bin)
}
