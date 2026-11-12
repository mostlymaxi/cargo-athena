//! `-a name=value` / `--input-file` parsing, and the type-check of the
//! values against a template's real signature.

use cargo_athena::{ContainerRunMeta, serde_json};
use std::collections::BTreeMap;
use std::path::Path;

/// `-a name=value` (JSON-else-string) merged over a `--input-file`
/// JSON object.
pub(crate) fn parse_args(
    input_file: Option<&Path>,
    kvs: &[String],
) -> Result<BTreeMap<String, serde_json::Value>, String> {
    let mut vals: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    if let Some(f) = input_file {
        let txt =
            std::fs::read_to_string(f).map_err(|e| format!("--input-file {}: {e}", f.display()))?;
        match serde_json::from_str::<serde_json::Value>(&txt) {
            Ok(serde_json::Value::Object(m)) => vals.extend(m),
            _ => return Err("--input-file must be a JSON object".to_string()),
        }
    }
    for kv in kvs {
        let (k, v) = kv
            .split_once('=')
            .ok_or_else(|| format!("-a expects name=value, got {kv:?}"))?;
        let val = serde_json::from_str::<serde_json::Value>(v)
            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
        vals.insert(k.to_string(), val);
    }
    Ok(vals)
}

/// Type-check supplied values against a template's real signature:
/// missing required, unknown (with did-you-mean), and wrong
/// scalar/array kinds. `Ok` if clean; `Err` is a ready-to-print
/// CLI-style report (the caller prefixes + exits).
pub(crate) fn validate_args(
    meta: &ContainerRunMeta,
    vals: &BTreeMap<String, serde_json::Value>,
) -> Result<(), String> {
    let declared: std::collections::BTreeSet<&str> =
        meta.params.iter().map(|p| p.name.as_str()).collect();
    let mut missing = Vec::new();
    let mut mism = Vec::new();
    for p in &meta.params {
        let norm: String = p.ty.split_whitespace().collect();
        let (inner, optional) = match norm
            .strip_prefix("Option<")
            .and_then(|s| s.strip_suffix('>'))
        {
            Some(i) => (i.to_string(), true),
            None => (norm.clone(), false),
        };
        match vals.get(&p.name) {
            None if optional => {}
            None => missing.push(format!(
                "    {}: {}",
                p.name,
                if p.ty.is_empty() { "?" } else { &p.ty }
            )),
            Some(v) => {
                if let Some(exp) = expected_kind(&inner)
                    && !kind_ok(exp, v)
                {
                    mism.push(format!(
                        "    {}: expected {} ({}), got {} {}",
                        p.name,
                        exp,
                        p.ty,
                        json_kind(v),
                        preview(v),
                    ));
                }
            }
        }
    }
    let unknown: Vec<String> = vals
        .keys()
        .filter(|k| !declared.contains(k.as_str()))
        .map(|k| match suggest(k, &declared) {
            Some(s) => format!("    {k}  (did you mean `{s}`?)"),
            None => format!("    {k}"),
        })
        .collect();

    if missing.is_empty() && mism.is_empty() && unknown.is_empty() {
        return Ok(());
    }
    let mut m = format!("error: invalid arguments for `{}`\n", meta.name);
    if !missing.is_empty() {
        m.push_str("\n  missing required parameter(s):\n");
        m.push_str(&missing.join("\n"));
        m.push('\n');
    }
    if !mism.is_empty() {
        m.push_str("\n  type mismatch:\n");
        m.push_str(&mism.join("\n"));
        m.push('\n');
    }
    if !unknown.is_empty() {
        m.push_str(&format!(
            "\n  unknown parameter(s) (not an input of `{}`):\n",
            meta.name
        ));
        m.push_str(&unknown.join("\n"));
        m.push('\n');
    }
    let sig: Vec<String> = meta
        .params
        .iter()
        .map(|p| {
            if p.ty.is_empty() {
                p.name.clone()
            } else {
                format!("{}: {}", p.name, p.ty)
            }
        })
        .collect();
    m.push_str(&format!("\n  expected inputs: {}\n", sig.join(", ")));
    m.push_str("  pass with -a <name>=<value> (JSON value, else string) or --input-file");
    Err(m)
}

/// The JSON shape a scalar/array Rust type round-trips from.
#[derive(Clone, Copy, PartialEq)]
enum Kind {
    Str,
    Int,
    Float,
    Bool,
    Arr,
}
impl std::fmt::Display for Kind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Kind::Str => "string",
            Kind::Int => "integer",
            Kind::Float => "number",
            Kind::Bool => "bool",
            Kind::Arr => "array",
        })
    }
}

/// Map a normalized (whitespace-stripped) Rust type to the JSON kind it
/// (de)serializes from. `None` ⇒ a struct/enum/map/tuple/generic we
/// don't shape-check here — the container still does full serde.
fn expected_kind(ty: &str) -> Option<Kind> {
    let t = ty.trim_start_matches('&');
    let t = t
        .strip_prefix("'static")
        .unwrap_or(t)
        .trim_start_matches('&');
    match t {
        "String" | "str" | "char" | "PathBuf" | "Path" | "Box<str>" => Some(Kind::Str),
        "i8" | "i16" | "i32" | "i64" | "i128" | "isize" | "u8" | "u16" | "u32" | "u64" | "u128"
        | "usize" => Some(Kind::Int),
        "f32" | "f64" => Some(Kind::Float),
        "bool" => Some(Kind::Bool),
        _ if t.starts_with("Vec<")
            || t.starts_with("VecDeque<")
            || t.starts_with('[')
            || t.starts_with("&[") =>
        {
            Some(Kind::Arr)
        }
        _ if t.contains("Cow<") && t.contains("str") => Some(Kind::Str),
        _ => None,
    }
}

fn kind_ok(k: Kind, v: &serde_json::Value) -> bool {
    match k {
        Kind::Str => v.is_string(),
        Kind::Int => v.is_i64() || v.is_u64(),
        Kind::Float => v.is_number(),
        Kind::Bool => v.is_boolean(),
        Kind::Arr => v.is_array(),
    }
}

fn json_kind(v: &serde_json::Value) -> &'static str {
    match v {
        serde_json::Value::Null => "null",
        serde_json::Value::Bool(_) => "bool",
        serde_json::Value::Number(n) if n.is_f64() => "number",
        serde_json::Value::Number(_) => "integer",
        serde_json::Value::String(_) => "string",
        serde_json::Value::Array(_) => "array",
        serde_json::Value::Object(_) => "object",
    }
}

fn preview(v: &serde_json::Value) -> String {
    let s = v.to_string();
    // Truncate on a char boundary — a byte slice would panic mid-error-
    // report when a multi-byte char straddles the cut.
    match s.char_indices().nth(40) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// Cheap typo hint: a declared name within edit-distance 2.
fn suggest(got: &str, declared: &std::collections::BTreeSet<&str>) -> Option<String> {
    declared
        .iter()
        .map(|d| (edit_distance(got, d), d.to_string()))
        .filter(|(d, _)| *d <= 2)
        .min_by_key(|(d, _)| *d)
        .map(|(_, s)| s)
}

fn edit_distance(a: &str, b: &str) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.iter().enumerate() {
        let mut cur = vec![i + 1];
        for (j, cb) in b.iter().enumerate() {
            let sub = prev[j] + usize::from(ca != cb);
            cur.push(sub.min(prev[j + 1] + 1).min(cur[j] + 1));
        }
        prev = cur;
    }
    prev[b.len()]
}
