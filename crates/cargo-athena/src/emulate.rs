//! `cargo athena container emulate` — realize ONE `#[container]`'s
//! emitted spec locally under docker/podman, *exactly as Argo would*: same
//! image, the same injected bootstrap, the same `ATHENA_PARAM_*` env,
//! the same `/athena` scratch dir, `host!` binds, and S3 artifact ports.
//!
//! Fidelity is by construction: the binary reports a [`ContainerRunMeta`]
//! derived from the *same* `Template::build()` that `emit` uses (via
//! `CARGO_ATHENA_DESCRIBE`), so there is nothing to keep in sync.
//!
//! By default the binary is **pulled from the deployed S3 tarball**, so
//! you can smoke-test what's live with no source on the node. `--build`
//! packages a local musl binary instead; `--tarball` takes one as-is.
//!
//! Limitations (no Kubernetes here): a `#[container(service_account=…)]`
//! and any podSpec-level concerns (RBAC, `nodeSelector`, podSpecPatch)
//! are **not** emulated — `docker run` has no notion of them. This runs
//! the container body faithfully, not the pod's k8s context.

use cargo_athena::{ContainerRunMeta, S3Ref, serde_json};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::{Command, exit};

use crate::pkg::PkgSel;

#[derive(clap::Args)]
pub struct EmulateArgs {
    /// Template name (`<crate>-<fn>` kebab, or the
    /// `#[container(name = "…")]` override). `container ls` lists them.
    template: String,
    #[command(flatten)]
    pkg: PkgSel,
    /// Set one input parameter: `-a name=value`. `value` is parsed as
    /// JSON if it parses (so `-a n=4` is the number 4, `-a b=true` a
    /// bool), else treated as a string. Repeatable.
    #[arg(short = 'a', long = "arg", value_name = "NAME=VALUE")]
    args: Vec<String>,
    /// JSON object of the function arguments (merged under `-a`).
    #[arg(long = "input-file", value_name = "FILE")]
    input_file: Option<PathBuf>,
    /// Build a local musl binary instead of pulling the deployed S3
    /// tarball (packages only the host-arch musl target — for the full
    /// multi-arch deployed artifact, `cargo athena build` then pull).
    #[arg(long, conflicts_with = "tarball")]
    build: bool,
    /// Use this tarball verbatim (skip pull/build).
    #[arg(long, value_name = "FILE", conflicts_with = "build")]
    tarball: Option<PathBuf>,
    /// `docker` | `podman`. Default: autodetect (prefer docker).
    #[arg(long)]
    runtime: Option<String>,
    /// Don't sync `load_artifact!`/`save_artifact!` ports to/from S3.
    #[arg(long = "skip-artifacts")]
    skip_artifacts: bool,
}

#[derive(clap::Args)]
pub struct DescribeArgs {
    /// Template name (`<crate>-<fn>` kebab, or the
    /// `#[container(name = "…")]` override).
    template: String,
    #[command(flatten)]
    pkg: PkgSel,
}

#[derive(clap::Args)]
pub struct LsArgs {
    #[command(flatten)]
    pkg: PkgSel,
    /// Include `#[workflow]`s and synthetic templates too (default:
    /// only `#[container]`s — the things `emulate` runs).
    #[arg(long)]
    all: bool,
}

#[derive(clap::Args)]
pub struct WorkflowLsArgs {
    #[command(flatten)]
    pkg: PkgSel,
    /// Also list athena-synthesized `if`/`else` wrapper + arm
    /// sub-workflows (an implementation detail, hidden by default).
    #[arg(long)]
    include_synthetic: bool,
}

/// `container describe` / `workflow describe` — print the metadata one
/// template reports (image, params+types, the binary/`host!`/artifact
/// ports, scratch + result paths). Exactly what `emulate` consumes.
pub fn describe_print(a: DescribeArgs) {
    let (pkg, bin) = a.pkg.resolve();
    let meta = describe(&a.template, pkg.as_deref(), bin.as_deref());
    println!(
        "{}",
        serde_json::to_string_pretty(&meta).expect("ContainerRunMeta is serializable")
    );
}

/// Spawn the workflow binary in list-mode and parse every template's
/// metadata (shared by `container ls` and `workflow ls`).
fn fetch_list(pkg: Option<&str>, bin: Option<&str>) -> Vec<ContainerRunMeta> {
    let mut cmd = crate::cargo_run(pkg, bin);
    cmd.env("CARGO_ATHENA_LIST", "1");
    let out = cmd
        .output()
        .unwrap_or_else(|e| die(&format!("failed to spawn `cargo run`: {e}")));
    if !out.status.success() || out.stdout.is_empty() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        die("could not list templates (run from your workflow crate, or pass --package/--bin)");
    }
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| die(&format!("could not parse template list ({e})")))
}

/// Render a `NAME  KIND  ARGS` table (rows pre-filtered + sorted).
fn print_table(mut rows: Vec<&ContainerRunMeta>) {
    rows.sort_by(|x, y| x.name.cmp(&y.name));
    if rows.is_empty() {
        eprintln!("(no matching templates)");
        return;
    }
    let w = rows.iter().map(|m| m.name.len()).max().unwrap_or(4).max(4);
    println!("{:<w$}  KIND       ARGS", "NAME", w = w);
    for m in rows {
        let sig = m
            .params
            .iter()
            .map(|p| {
                if p.ty.is_empty() {
                    p.name.clone()
                } else {
                    format!("{}: {}", p.name, p.ty)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        println!("{:<w$}  {:<9}  {sig}", m.name, m.kind, w = w);
    }
}

/// `cargo athena container ls` — discoverable names for
/// `emulate`/`describe`. Containers only unless `--all`.
pub fn container_ls(a: LsArgs) {
    let (pkg, bin) = a.pkg.resolve();
    let all = fetch_list(pkg.as_deref(), bin.as_deref());
    print_table(
        all.iter()
            .filter(|m| a.all || m.kind == "container")
            .collect(),
    );
}

/// `cargo athena workflow ls` — the `#[workflow]`s in the package
/// (synthetic `if` wrappers/arms hidden unless `--include-synthetic`).
pub fn workflow_ls(a: WorkflowLsArgs) {
    let (pkg, bin) = a.pkg.resolve();
    let all = fetch_list(pkg.as_deref(), bin.as_deref());
    print_table(
        all.iter()
            .filter(|m| m.kind == "workflow" && (a.include_synthetic || !m.synthetic))
            .collect(),
    );
}

fn die(msg: &str) -> ! {
    eprintln!("cargo athena container emulate: {msg}");
    exit(2);
}

pub fn container_emulate(a: EmulateArgs) {
    let (pkg, bin) = a.pkg.resolve();
    // 1. Introspect — the binary builds the *same* template `emit` does
    //    and reports it in the runner's vocabulary.
    let meta = describe(&a.template, pkg.as_deref(), bin.as_deref());
    if meta.kind != "container" {
        die(&format!(
            "{:?} is a #[{}]; `container emulate` targets a single #[container]. \
             A #[workflow] is a DAG with no pod — run its containers individually.",
            meta.name, meta.kind
        ));
    }

    // 2. Type-check the supplied arguments against the container's real
    //    fn signature *before* doing any work (S3 pull, docker) — a typo
    //    or wrong type fails fast with a CLI-style error, not a cryptic
    //    serde panic three steps later inside the pod.
    let values = check_params(&meta, &a);

    // 3. Container runtime.
    let runtime = detect_runtime(a.runtime.as_deref());

    // 4. Host scratch dir, bind-mounted at the pod's emptyDir path; all
    //    athena-managed paths (binary, artifacts, result) live under it.
    let work = scratch_dir(&meta.name);
    let host_of = |cpath: &str| -> PathBuf {
        let rel = cpath
            .strip_prefix(&meta.work_dir)
            .unwrap_or(cpath)
            .trim_start_matches('/');
        work.join(rel)
    };

    // Binary: pull (default) / build / explicit.
    let tarball = match (&a.tarball, a.build) {
        (Some(p), _) => p.clone(),
        (None, true) => build_local(pkg.as_deref(), bin.as_deref()),
        (None, false) => {
            let ba = meta.binary_artifact.as_ref().unwrap_or_else(|| {
                die(
                    "template has no binary artifact (run `cargo athena build` first, \
                     or pass --build / --tarball)",
                )
            });
            let dst = work.join("dist.tar.gz");
            s3_get(&ba.s3, &dst);
            dst
        }
    };
    if let Some(ba) = &meta.binary_artifact {
        // Argo's executor init container auto-extracts the input tarball
        // into the artifact `path`. Mimic Argo's `unpack` semantics 1:1
        // on the host bind-mount so the emulated bootstrap finds
        // `app-<triple>` at the same paths it would in-pod (zero-drift).
        // Pure-Rust tar+flate2 — no host `tar` dependency.
        let dst = host_of(&ba.path);
        crate::tarball::extract_argo_compat(&tarball, &dst).unwrap_or_else(|e| {
            die(&format!(
                "extract {} into {}: {e}",
                tarball.display(),
                dst.display()
            ))
        });
    }

    // Input artifact ports ← S3.
    if !a.skip_artifacts {
        for art in &meta.input_artifacts {
            let dst = host_of(&art.path);
            mkparent(&dst);
            s3_get(&art.s3, &dst);
        }
    }

    // docker/podman run — image + the emitted bootstrap verbatim.
    let mut c = Command::new(&runtime);
    c.arg("run").arg("--rm");
    c.arg("-v")
        .arg(format!("{}:{}", work.display(), meta.work_dir));
    for hp in &meta.host_paths {
        if !Path::new(hp).exists() {
            eprintln!("warning: host! path {hp} doesn't exist locally; binding anyway");
        }
        // Mirror the in-pod mount path (safe-by-construction; see
        // `cargo_athena::host_mount_path` / `ATHENA_MOUNTS_DIR`). Never
        // bind a host path at the same in-container path — `host!("/")`
        // would otherwise mount the host root over the container's.
        let mount = cargo_athena::host_mount_path(hp);
        c.arg("-v").arg(format!("{hp}:{mount}"));
    }
    for (name, json) in &values {
        c.arg("-e").arg(format!("{name}={json}"));
    }
    // Argo sets the container `command` (→ overrides the image
    // ENTRYPOINT) + `args`. Mirror that with --entrypoint so the
    // injected bootstrap runs exactly as in-pod.
    let (entry, rest) = meta
        .command
        .split_first()
        .unwrap_or_else(|| die("template has no container command"));
    c.arg("--entrypoint").arg(entry);
    c.arg(&meta.image);
    c.args(rest);
    c.args(&meta.args);

    eprintln!("→ {runtime} run {} ({})", meta.image, meta.name);
    let status = c
        .status()
        .unwrap_or_else(|e| die(&format!("failed to start {runtime}: {e}")));

    // 6. Output artifact ports → S3.
    if !a.skip_artifacts {
        for art in &meta.output_artifacts {
            let src = host_of(&art.path);
            if src.exists() {
                s3_put(&art.s3, &src);
            }
        }
    }

    // 7. Surface the body's return value (Argo's outputs.parameters.return).
    if let Some(rp) = &meta.result_path
        && let Ok(s) = std::fs::read_to_string(host_of(rp))
    {
        match serde_json::from_str::<serde_json::Value>(s.trim()) {
            Ok(v) => println!(
                "return: {}",
                serde_json::to_string_pretty(&v).unwrap_or_else(|_| s.clone())
            ),
            Err(_) => println!("return: {}", s.trim()),
        }
    }
    let _ = std::fs::remove_dir_all(&work);
    exit(status.code().unwrap_or(1));
}

/// Spawn the user binary in describe-mode and parse its metadata.
///
/// The binary is *your workflow crate's* (the one whose `main` calls
/// `cargo_athena::entrypoint!(Root)`). Run from that crate, or pass
/// `--package`/`--bin`. A common gotcha: running inside the
/// `cargo-athena` crate itself makes `cargo run` build the CLI, not a
/// workflow — hence the explicit guidance below instead of dumping its
/// clap usage.
pub(crate) fn describe(
    template: &str,
    package: Option<&str>,
    bin: Option<&str>,
) -> ContainerRunMeta {
    let hint = || -> String {
        let where_ = match (package, bin) {
            (Some(p), Some(b)) => format!("--package {p} --bin {b}"),
            (Some(p), None) => format!("--package {p}"),
            (None, Some(b)) => format!("--bin {b}"),
            (None, None) => "this directory".to_string(),
        };
        format!(
            "could not get container metadata from your workflow binary ({where_}).\n\
             \x20 - run this from your workflow crate, or pass --package/--bin;\n\
             \x20 - its `main` must call `cargo_athena::entrypoint!(Root)`;\n\
             \x20 - {template:?} must be a template reachable from that root \
             (`<crate>-<fn>` kebab, or the #[container(name=…)] override)."
        )
    };
    let mut cmd = crate::cargo_run(package, bin);
    cmd.env("CARGO_ATHENA_DESCRIBE", template);
    let out = cmd
        .output()
        .unwrap_or_else(|e| die(&format!("failed to spawn `cargo run`: {e}\n{}", hint())));
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        die(&format!("{}\n\n  binary output (tail):\n{tail}", hint()));
    }
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        die(&format!(
            "could not parse container metadata ({e}).\n{}",
            hint()
        ))
    })
}

fn detect_runtime(over: Option<&str>) -> String {
    if let Some(r) = over {
        if !crate::tool_ok(r, &["--version"]) {
            die(&format!("--runtime {r:?} is not runnable"));
        }
        return r.to_string();
    }
    for r in ["docker", "podman"] {
        if crate::tool_ok(r, &["--version"]) {
            return r.to_string();
        }
    }
    die("neither `docker` nor `podman` found on PATH — install one, or pass --runtime");
}

fn scratch_dir(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("athena-run-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap_or_else(|e| die(&format!("mkdir scratch: {e}")));
    d
}

fn mkparent(p: &Path) {
    if let Some(d) = p.parent() {
        std::fs::create_dir_all(d).unwrap_or_else(|e| die(&format!("mkdir {}: {e}", d.display())));
    }
}

/// Parse `-a name=value` (JSON-else-string) merged over `--input-file`,
/// then **type-check against the container fn's real signature** before
/// anything launches: missing required args, unknown args (typos), and
/// wrong scalar/array kinds all fail fast with one CLI-style report.
/// On success, returns the params JSON-encoded per Regime B (string →
/// `"v"`, number → `7`) into the env each is delivered through.
fn check_params(meta: &ContainerRunMeta, a: &EmulateArgs) -> Vec<(String, String)> {
    let vals = parse_args(a.input_file.as_deref(), &a.args);
    if let Err(report) = validate_args(meta, &vals) {
        die(&report);
    }
    // emulate delivers each param through its env var.
    meta.params
        .iter()
        .filter_map(|p| {
            vals.get(&p.name).map(|v| {
                (
                    p.env.clone(),
                    serde_json::to_string(v).expect("JSON-encodable param"),
                )
            })
        })
        .collect()
}

/// `-a name=value` (JSON-else-string) merged over a `--input-file`
/// JSON object. Shared by `emulate` and `submit`.
pub(crate) fn parse_args(
    input_file: Option<&Path>,
    kvs: &[String],
) -> BTreeMap<String, serde_json::Value> {
    let mut vals: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    if let Some(f) = input_file {
        let txt = std::fs::read_to_string(f)
            .unwrap_or_else(|e| die(&format!("--input-file {}: {e}", f.display())));
        match serde_json::from_str::<serde_json::Value>(&txt) {
            Ok(serde_json::Value::Object(m)) => vals.extend(m),
            _ => die("--input-file must be a JSON object"),
        }
    }
    for kv in kvs {
        let (k, v) = kv
            .split_once('=')
            .unwrap_or_else(|| die(&format!("-a expects name=value, got {kv:?}")));
        let val = serde_json::from_str::<serde_json::Value>(v)
            .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
        vals.insert(k.to_string(), val);
    }
    vals
}

/// Type-check supplied values against a template's real signature:
/// missing required, unknown (with did-you-mean), and wrong
/// scalar/array kinds. `Ok` if clean; `Err` is a ready-to-print
/// CLI-style report (the caller prefixes + exits). Shared by `emulate`
/// and `submit`.
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
    if s.len() > 40 {
        format!("{}…", &s[..40])
    } else {
        s
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

// ---- S3 (object_store, lean current-thread runtime) -----------------------

pub(crate) fn s3_store(s3: &S3Ref) -> object_store::aws::AmazonS3 {
    use object_store::aws::AmazonS3Builder;
    let mut b = AmazonS3Builder::new()
        .with_bucket_name(&s3.bucket)
        .with_region(&s3.region)
        .with_allow_http(s3.insecure);
    // `AWS_ENDPOINT_URL` (AWS-SDK standard) overrides the config
    // endpoint — needed when S3 is reached differently from here than
    // from the pods (e.g. an in-cluster DNS in `athena.toml` for the
    // bootstrap, but a port-forward / public host when `publish`ing
    // from outside the cluster). Falls back to `AWS_ENDPOINT_URL_S3`.
    let ep_env = std::env::var("AWS_ENDPOINT_URL")
        .ok()
        .or_else(|| std::env::var("AWS_ENDPOINT_URL_S3").ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(url) = ep_env {
        if url.starts_with("http://") {
            b = b.with_allow_http(true);
        }
        b = b.with_endpoint(url);
    } else {
        // AWS proper: let region drive the URL. Custom (MinIO/etc.): use
        // the endpoint, adding a scheme if the config gave a bare host.
        let ep = s3.endpoint.trim();
        if !ep.is_empty() && !ep.ends_with("amazonaws.com") {
            let url = if ep.contains("://") {
                ep.to_string()
            } else if s3.insecure {
                format!("http://{ep}")
            } else {
                format!("https://{ep}")
            };
            b = b.with_endpoint(url);
        }
    }
    // Credentials: the standard AWS env vars below, else object_store's
    // ambient fallback (EC2 IMDS / ECS task role / IRSA web-identity).
    // object_store does NOT parse `~/.aws/credentials` or `AWS_PROFILE`
    // — the shared-config file is unsupported here (not the AWS SDK).
    if let Ok(v) = std::env::var("AWS_ACCESS_KEY_ID") {
        b = b.with_access_key_id(v);
    }
    if let Ok(v) = std::env::var("AWS_SECRET_ACCESS_KEY") {
        b = b.with_secret_access_key(v);
    }
    if let Ok(v) = std::env::var("AWS_SESSION_TOKEN") {
        b = b.with_token(v);
    }
    b.build()
        .unwrap_or_else(|e| die(&format!("S3 client for bucket {:?}: {e}", s3.bucket)))
}

pub(crate) fn rt() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| die(&format!("async runtime: {e}")))
}

/// Does the S3 object exist? (`cargo athena submit` pre-flights the
/// binary tarball so it doesn't submit a workflow whose pods can't
/// bootstrap.)
pub(crate) fn s3_exists(s3: &S3Ref) -> bool {
    let store = s3_store(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    rt().block_on(async { object_store::ObjectStore::head(&store, &key).await })
        .is_ok()
}

fn s3_get(s3: &S3Ref, dst: &Path) {
    let store = s3_store(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    let bytes = rt()
        .block_on(async {
            let r = object_store::ObjectStore::get(&store, &key).await?;
            r.bytes().await
        })
        .unwrap_or_else(|e| die(&format!("S3 GET {}: {e}", s3.key)));
    mkparent(dst);
    std::fs::write(dst, &bytes).unwrap_or_else(|e| die(&format!("write {}: {e}", dst.display())));
}

pub(crate) fn s3_put(s3: &S3Ref, src: &Path) {
    let store = s3_store(s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    let data = std::fs::read(src).unwrap_or_else(|e| die(&format!("read {}: {e}", src.display())));
    rt().block_on(async { object_store::ObjectStore::put(&store, &key, data.into()).await })
        .unwrap_or_else(|e| die(&format!("S3 PUT {}: {e}", s3.key)));
}

// ---- --build (local, host-arch musl only) ---------------------------------

fn build_local(package: Option<&str>, bin: Option<&str>) -> PathBuf {
    crate::preflight_zig();
    let (krate, _ver, default_bin) = crate::package_meta(package);
    let bin = bin.map(str::to_string).unwrap_or(default_bin);
    let triple = if cfg!(target_arch = "aarch64") {
        "aarch64-unknown-linux-musl"
    } else {
        "x86_64-unknown-linux-musl"
    };
    let st = Command::new("cargo")
        .args([
            "zigbuild",
            "--release",
            "--target",
            triple,
            "-p",
            &krate,
            "--bin",
            &bin,
        ])
        .status()
        .unwrap_or_else(|e| die(&format!("cargo zigbuild: {e}")));
    if !st.success() {
        die("local build failed");
    }
    let stage = std::env::temp_dir().join(format!("athena-build-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&stage);
    std::fs::create_dir_all(&stage).ok();
    let from = std::path::PathBuf::from(format!("target/{triple}/release/{bin}"));
    let staged_bin = stage.join(format!("app-{triple}"));
    std::fs::copy(&from, &staged_bin)
        .unwrap_or_else(|e| die(&format!("copy {}: {e}", from.display())));
    // Pure-Rust pack — same `bin/<entry>` layout as `cargo athena
    // publish` (`tarball::create`), so the emulated tarball survives
    // Argo's single-entry-rename `unpack` quirk.
    let tb = stage.join("dist.tar.gz");
    let name = format!("app-{triple}");
    crate::tarball::create(&tb, &[(staged_bin.as_path(), name.as_str())])
        .unwrap_or_else(|e| die(&format!("create tarball: {e}")));
    tb
}
