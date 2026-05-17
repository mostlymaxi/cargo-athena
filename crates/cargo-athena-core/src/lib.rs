//! cargo-athena runtime.
//!
//! Every `#[workflow]`/`#[container]` becomes a unit-struct **type** that
//! implements [`Template`]. That type *is* the cross-crate wormhole: the
//! type system resolves a callee's Argo name/inputs across modules and
//! crates, and the generated `Template::collect` calls
//! `<Callee as Template>::collect` directly — a monomorphic call, so the
//! whole reachable closure is force-linked with no `inventory`/DCE games.
//!
//! Two worlds share one binary:
//!
//! * **Emit** — `main` calls [`entrypoint::<E>()`]; we walk the closure
//!   from `E` and print one Argo `WorkflowTemplate` document per template
//!   (cross-refs via `templateRef`) plus a runnable `Workflow` for `E`.
//! * **Run** — Argo invokes the binary with `--cargo-athena-template <name>`;
//!   we deserialize inputs, run the real container body, serialize outputs.
//!
//! `host!` declarations are still collected statically by the attribute
//! macros and resolved through the `#[fragment]` (`inventory`) closure here
//! — fragments are genuinely *called* by container bodies, so unlike
//! templates they have a real symbol reference and no DCE concern.

use std::collections::{HashMap, HashSet};

/// Argo API types (generated from protobuf).
pub use cargo_athena_api as api;
// Re-exported so macro-generated code has stable paths under `::cargo_athena`.
pub use inventory;
pub use serde_json;
// Maintained serde_yaml fork: YAML 1.1-aware emitter (quotes `n`/`yes`/
// `null`/… so Argo's Go YAML→JSON parser can't mis-type them). serde_yaml
// itself is archived/EOL.
pub use serde_norway;

/// Fan-out: `list.fan_out(|x| template(x, ..))` runs `template` once per
/// element (Argo `withParam`); the binding is the aggregated `Vec<U>` of
/// the per-element returns. This trait exists only so the ghost
/// type-checks the element type, the closure, and the resulting
/// `Vec<U>`; the macro lowers the call to Argo and it never runs.
pub trait AthenaList<T> {
    #[doc(hidden)]
    fn fan_out<U, F: FnOnce(T) -> U>(self, _f: F) -> Vec<U>
    where
        Self: Sized,
    {
        unimplemented!("athena ghost: never executed")
    }
}
impl<T> AthenaList<T> for Vec<T> {}
impl<T, const N: usize> AthenaList<T> for [T; N] {}

/// Marker for types that may be injected into a `#[container]`
/// attribute (`image = "repo:" + tag`). Restricted to `String`/`str`
/// and the primitive numbers: their `serde_json` form, unwrapped at
/// runtime by `{{=fromJSON(...)}}`, renders to the obvious raw scalar
/// (`"v"`→`v`, `7`→`7`). A `Display` bound would wrongly admit types
/// whose `Display` differs from their JSON round-trip. The macro emits
/// a hidden `Injectable`-bounded assertion against the real arg type.
#[doc(hidden)]
pub trait Injectable {}
macro_rules! __athena_injectable {
    ($($t:ty),* $(,)?) => { $( impl Injectable for $t {} )* };
}
__athena_injectable!(
    String, str, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64,
    u128, usize, f32, f64,
);

/// `host!("/lit/path")` — declare a hostPath volume for the enclosing
/// container, evaluating to the (already-mounted) path at runtime.
///
/// Only valid inside a `#[cargo_athena::container]` or
/// `#[cargo_athena::fragment]` fn: those attribute macros rewrite the
/// invocations they can see into the private real macro below. This
/// public definition is therefore a *hard error* — it only ever expands
/// when `host!` is used somewhere the collector cannot see it (a plain
/// fn, a `#[workflow]`, or nested inside another macro's tokens), where a
/// silently-unmounted path would otherwise be a footgun.
#[macro_export]
macro_rules! host {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`host!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn \
             (not in a plain fn, a `#[workflow]`, or nested inside another \
             macro invocation)"
        )
    };
}

/// The real expansion. Private: only the attribute macros emit this path.
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_host {
    ($path:literal) => {
        $crate::rt::host_path($path)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("`host!` takes a single string-literal path")
    };
}

// --- artifact declaration macros (native Argo artifacts, no S3) ------------
//
// Each is a public (gated `compile_error!`) + private (real) pair, exactly
// like `host!`: only valid inside `#[container]`/`#[fragment]`, where the
// attribute macro rewrites the public form to the private one.

/// Declare an Argo *input* artifact port and read it (bytes) at runtime.
#[macro_export]
macro_rules! load_artifact {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`load_artifact!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_load_artifact {
    ($name:literal) => { $crate::rt::load_artifact($name) };
    ($($t:tt)*) => { ::core::compile_error!("load_artifact!(\"name\")") };
}

/// Declare an Argo *input* artifact port and read it (UTF-8) at runtime.
#[macro_export]
macro_rules! load_artifact_str {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`load_artifact_str!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_load_artifact_str {
    ($name:literal) => { $crate::rt::load_artifact_str($name) };
    ($($t:tt)*) => { ::core::compile_error!("load_artifact_str!(\"name\")") };
}

/// Declare an Argo *output* artifact port and write bytes to it at runtime.
#[macro_export]
macro_rules! save_artifact {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`save_artifact!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_save_artifact {
    ($name:literal, $data:expr) => { $crate::rt::save_artifact($name, $data) };
    ($($t:tt)*) => { ::core::compile_error!("save_artifact!(\"name\", data)") };
}

/// Declare an Argo *output* artifact port and write a string at runtime.
#[macro_export]
macro_rules! save_artifact_str {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`save_artifact_str!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_save_artifact_str {
    ($name:literal, $data:expr) => { $crate::rt::save_artifact_str($name, $data) };
    ($($t:tt)*) => { ::core::compile_error!("save_artifact_str!(\"name\", data)") };
}

/// Runtime shims referenced by the declaration macros. Artifact ports are
/// plain files at fixed paths; Argo moves them (no S3 from us).
pub mod rt {
    use std::path::PathBuf;

    /// Identity: the volume is already mounted when the container runs.
    pub const fn host_path(path: &'static str) -> &'static str {
        path
    }

    /// Where Argo drops/collects declared artifact ports inside the pod.
    pub const IN_DIR: &str = "/athena/artifacts/in";
    pub const OUT_DIR: &str = "/athena/artifacts/out";

    fn in_path(name: &str) -> PathBuf {
        PathBuf::from(IN_DIR).join(name)
    }
    fn out_path(name: &str) -> PathBuf {
        PathBuf::from(OUT_DIR).join(name)
    }

    pub fn load_artifact(name: &str) -> Vec<u8> {
        let p = in_path(name);
        std::fs::read(&p).unwrap_or_else(|e| panic!("load_artifact({name:?}) {}: {e}", p.display()))
    }

    pub fn load_artifact_str(name: &str) -> String {
        let p = in_path(name);
        std::fs::read_to_string(&p)
            .unwrap_or_else(|e| panic!("load_artifact_str({name:?}) {}: {e}", p.display()))
    }

    pub fn save_artifact(name: &str, data: impl AsRef<[u8]>) {
        let p = out_path(name);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).expect("create artifact out dir");
        }
        std::fs::write(&p, data.as_ref())
            .unwrap_or_else(|e| panic!("save_artifact({name:?}) {}: {e}", p.display()));
    }

    pub fn save_artifact_str(name: &str, data: impl AsRef<str>) {
        save_artifact(name, data.as_ref().as_bytes());
    }
}

/// What kind of Argo template a type produces.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum TemplateKind {
    /// Leaf — real code in a pod (`#[container]`).
    Container,
    /// Composition — a DAG of other templates (`#[workflow]`).
    Workflow,
}

/// The cross-crate identity of a template, implemented by the unit struct
/// the `#[workflow]`/`#[container]` macros generate.
///
/// Callers never name the Argo string; they reference the *type*
/// (`<foo::ingest as Template>::ARGO_NAME`), so name/input resolution is
/// done by the compiler — collision-proof across crates, and the reference
/// itself force-links the defining crate.
pub trait Template {
    /// Globally-unique Argo resource name (`<crate>-<fn>` by default).
    const ARGO_NAME: &'static str;
    /// Declared input parameter names, in order.
    const INPUTS: &'static [&'static str];
    const KIND: TemplateKind;
    /// Whole-workflow exit-handler template name, from
    /// `#[workflow(on_exit=…)]`/`#[container(on_exit=…)]`. Only the emit
    /// root becomes a runnable `Workflow`, so only the root's takes
    /// effect (Argo `spec.onExit` is entrypoint/workflow-scoped) —
    /// non-root values are naturally inert.
    const ON_EXIT: Option<&'static str> = None;

    /// Build this template's inner Argo `template` object.
    fn build(ctx: &BuildCtx) -> api::Template;

    /// Run-mode body — overridden by `#[container]`; never called on a
    /// `#[workflow]`.
    fn run(_input: serde_json::Value) -> serde_json::Value {
        panic!("`{}` is not a #[container]; nothing to run", Self::ARGO_NAME)
    }

    /// Push self + the transitive callee closure into `out`. The macro
    /// generates `<Callee as Template>::collect(out)` per callee, so the
    /// whole reachable set is linked by direct calls.
    fn collect(out: &mut Collector);
}

// ---- athena.toml ---------------------------------------------------------

/// `athena.toml` — required by `cargo athena` at emit time. Mirrors the
/// parts of Argo's S3 `ArtifactRepository` we inject, plus bootstrap config.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AthenaConfig {
    pub artifact_repository: ArtifactRepository,
    pub artifact: ArtifactSpec,
    #[serde(default)]
    pub bootstrap: Bootstrap,
    #[serde(default)]
    pub defaults: Defaults,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Defaults {
    /// Kubernetes ServiceAccount the workflow pods run as (so users bind
    /// their own RBAC). Per-`#[container(service_account=...)]` overrides.
    #[serde(default = "default_service_account")]
    pub service_account: String,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            service_account: default_service_account(),
        }
    }
}

fn default_service_account() -> String {
    "default".to_string()
}

/// Resolve a container template's ServiceAccount: the
/// `#[container(service_account=...)]` override, else `[defaults]`.
pub fn service_account(ctx: &BuildCtx, over: Option<&str>) -> String {
    over.map(str::to_string)
        .unwrap_or_else(|| ctx.config().defaults.service_account.clone())
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArtifactRepository {
    pub s3: S3Repo,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct S3Repo {
    pub endpoint: String,
    pub bucket: String,
    #[serde(default)]
    pub region: String,
    #[serde(default)]
    pub insecure: bool,
    pub access_key_secret: SecretRef,
    pub secret_key_secret: SecretRef,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct SecretRef {
    pub name: String,
    pub key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ArtifactSpec {
    /// Object key of the per-binary tarball (holds `app-<triple>` for every
    /// `bootstrap.targets`). `cargo athena build` fills any
    /// `{crate}`/`{version}`/`{bin}` placeholders before publish.
    pub key: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct Bootstrap {
    /// Fallback image when a `#[container]` doesn't set its own. Per-
    /// container `image` always wins (arbitrary by design); this is just
    /// the small default for containers that don't care.
    #[serde(default = "default_image")]
    pub default_image: String,
    /// Cross-compile / `uname` target matrix.
    #[serde(default = "default_targets")]
    pub targets: Vec<String>,
}

impl Default for Bootstrap {
    fn default() -> Self {
        Self {
            default_image: default_image(),
            targets: default_targets(),
        }
    }
}

fn default_image() -> String {
    "busybox:1.36-musl".to_string()
}

fn default_targets() -> Vec<String> {
    vec![
        "x86_64-unknown-linux-musl".to_string(),
        "aarch64-unknown-linux-musl".to_string(),
    ]
}

impl AthenaConfig {
    /// `ATHENA_CONFIG` override, else the nearest `athena.toml` walking up
    /// from the cwd. Only ever called during emit — the in-pod binary
    /// (run-mode) never needs `athena.toml`.
    pub fn load() -> Self {
        let path = std::env::var_os("ATHENA_CONFIG")
            .map(std::path::PathBuf::from)
            .or_else(Self::find_upwards)
            .expect(
                "athena.toml not found: set ATHENA_CONFIG or add athena.toml \
                 to the workspace (required by `cargo athena`)",
            );
        let text = std::fs::read_to_string(&path)
            .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
        toml::from_str(&text).unwrap_or_else(|e| panic!("parse {}: {e}", path.display()))
    }

    fn find_upwards() -> Option<std::path::PathBuf> {
        let mut d = std::env::current_dir().ok()?;
        loop {
            let p = d.join("athena.toml");
            if p.is_file() {
                return Some(p);
            }
            if !d.pop() {
                return None;
            }
        }
    }
}

/// Pod-scoped scratch root, backed by an `emptyDir` on every container
/// template so all athena paths are writable regardless of the image
/// (distroless / read-only rootfs) and shared with Argo's init/wait
/// containers for artifact load/collect.
pub const ATHENA_DIR: &str = "/athena";
/// Where the bootstrap extracts the per-arch binary.
pub const ATHENA_BIN_DIR: &str = "/athena/bin";
/// Where the binary tarball is delivered (under [`ATHENA_DIR`]).
pub const ARTIFACT_PATH: &str = "/athena/dist.tar.gz";
/// Name of the scratch `emptyDir` volume.
pub const SCRATCH_VOLUME: &str = "athena-work";

/// What [`container_delivery`] produces for one `#[container]` template.
pub struct ContainerDelivery {
    /// Resolved image: the `#[container(image=...)]` override, else
    /// `[bootstrap].default_image`.
    pub image: String,
    pub command: Vec<String>,
    pub args: Vec<String>,
    pub artifact: api::Artifact,
}

/// The arch-resolving bootstrap + the S3 binary artifact for one container
/// template. Called from macro-generated `Template::build` (emit only).
///
/// Runs inside the container's *arbitrary, user-chosen* image (it only
/// needs a POSIX `sh`/`tar`/`uname`). The image is multi-arch (kubelet
/// picks the node arch); the bootstrap `uname`s and `exec`s the matching
/// `app-<triple>` from the Argo-delivered tarball, replacing the shell so
/// the Rust binary is the container's main process.
pub fn container_delivery(
    ctx: &BuildCtx,
    argo_name: &str,
    image_override: Option<&str>,
) -> ContainerDelivery {
    let cfg = ctx.config();
    let s3 = &cfg.artifact_repository.s3;
    let image = image_override
        .map(str::to_string)
        .unwrap_or_else(|| cfg.bootstrap.default_image.clone());

    let mut arms = String::new();
    for triple in &cfg.bootstrap.targets {
        let arch = triple.split('-').next().unwrap_or(triple);
        let pat = if arch == "aarch64" { "aarch64|arm64" } else { arch };
        arms.push_str(&format!("  {pat}) __t={triple} ;;\n"));
    }

    // Extract into the `/athena` emptyDir (always writable, even on a
    // distroless / read-only-rootfs image) — no `mktemp`/`/tmp` dependency.
    let script = format!(
        "set -e\n\
         case \"$(uname -m)\" in\n\
         {arms}  *) echo \"athena: unsupported arch $(uname -m)\" >&2; exit 1 ;;\n\
         esac\n\
         mkdir -p {ATHENA_BIN_DIR}\n\
         tar -xzf {ARTIFACT_PATH} -C {ATHENA_BIN_DIR}\n\
         chmod +x {ATHENA_BIN_DIR}/app-$__t\n\
         export CARGO_ATHENA_OUTPUT=/athena/result\n\
         exec {ATHENA_BIN_DIR}/app-$__t --cargo-athena-template {argo_name}\n"
    );

    let artifact = api::Artifact {
        name: "athena-dist".to_string(),
        path: ARTIFACT_PATH.to_string(),
        s3: Some(s3_loc(s3, &cfg.artifact.key)),
        archive: Some(archive_none()),
        mode: None,
    };

    ContainerDelivery {
        image,
        command: vec!["/bin/sh".to_string(), "-c".to_string()],
        args: vec![script],
        artifact,
    }
}

/// A `#[fragment]`: a plain helper carrying `host!` decls. Still
/// `inventory`-based — a container's real body actually *calls* its
/// fragments, so the symbol reference exists and DCE is not a concern.
pub struct FragmentReg {
    pub rust_name: &'static str,
    pub host_paths: &'static [&'static str],
    pub in_artifacts: &'static [&'static str],
    pub out_artifacts: &'static [&'static str],
    pub callees: &'static [&'static str],
}
inventory::collect!(FragmentReg);

/// Fragment registry snapshot, passed to container `build`s.
pub struct BuildCtx {
    fragments: HashMap<&'static str, &'static FragmentReg>,
    config: AthenaConfig,
}

impl BuildCtx {
    /// Emit-only: gathers fragments AND loads `athena.toml`. Never called
    /// in run-mode, so the in-pod binary needs no `athena.toml`.
    pub fn collect() -> Self {
        let mut fragments = HashMap::new();
        for f in inventory::iter::<FragmentReg> {
            fragments.insert(f.rust_name, f);
        }
        Self {
            fragments,
            config: AthenaConfig::load(),
        }
    }

    pub fn config(&self) -> &AthenaConfig {
        &self.config
    }

    /// Own literal decls ∪ the transitive `#[fragment]` closure for one
    /// kind of declaration (deduped, stable order). `select` picks which
    /// `FragmentReg` slice to pull from a reached fragment.
    fn resolved(
        &self,
        own: &[&str],
        own_callees: &[&str],
        select: impl Fn(&FragmentReg) -> &'static [&'static str],
    ) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let mut push = |p: &str, out: &mut Vec<String>| {
            if seen.insert(p.to_string()) {
                out.push(p.to_string());
            }
        };
        for p in own {
            push(p, &mut out);
        }
        let mut queue: Vec<&str> = own_callees.to_vec();
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(c) = queue.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(f) = self.fragments.get(c) {
                for p in select(f) {
                    push(p, &mut out);
                }
                queue.extend(f.callees.iter().copied());
            }
        }
        out
    }

    /// hostPaths: own `host!`s ∪ fragment closure.
    pub fn resolved_host_paths(&self, own: &[&str], callees: &[&str]) -> Vec<String> {
        self.resolved(own, callees, |f| f.host_paths)
    }

    /// Input artifact ports: own `load_artifact*!`s ∪ fragment closure.
    pub fn resolved_in_artifacts(&self, own: &[&str], callees: &[&str]) -> Vec<String> {
        self.resolved(own, callees, |f| f.in_artifacts)
    }

    /// Output artifact ports: own `save_artifact*!`s ∪ fragment closure.
    pub fn resolved_out_artifacts(&self, own: &[&str], callees: &[&str]) -> Vec<String> {
        self.resolved(own, callees, |f| f.out_artifacts)
    }
}

fn archive_none() -> api::ArchiveStrategy {
    api::ArchiveStrategy {
        none: Some(api::NoneStrategy {}),
    }
}

/// Build an Argo S3 location (artifact-repository creds from `athena.toml`)
/// for an exact object `key`.
pub fn s3_loc(s3: &S3Repo, key: &str) -> api::S3Artifact {
    api::S3Artifact {
        endpoint: s3.endpoint.clone(),
        bucket: s3.bucket.clone(),
        region: s3.region.clone(),
        insecure: s3.insecure,
        key: key.to_string(),
        access_key_secret: Some(api::SecretKeySelector {
            name: s3.access_key_secret.name.clone(),
            key: s3.access_key_secret.key.clone(),
        }),
        secret_key_secret: Some(api::SecretKeySelector {
            name: s3.secret_key_secret.name.clone(),
            key: s3.secret_key_secret.key.clone(),
        }),
    }
}

/// A valid Argo artifact identifier derived from an S3 key (which may
/// contain `/`, `.`). The key itself is preserved in `s3.key`.
fn artifact_ident(key: &str) -> String {
    let mut s: String = key
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    s = s.trim_matches('-').to_ascii_lowercase();
    if s.is_empty() {
        s.push('a');
    }
    s
}

/// `load_artifact!("key")` input ports: Argo pulls the exact S3 object
/// `key` from the configured repo into the pod (raw, `archive: none`).
pub fn artifact_inputs(ctx: &BuildCtx, keys: &[String]) -> Vec<api::Artifact> {
    let s3 = &ctx.config().artifact_repository.s3;
    keys.iter()
        .map(|k| api::Artifact {
            name: artifact_ident(k),
            path: format!("{}/{k}", rt::IN_DIR),
            s3: Some(s3_loc(s3, k)),
            archive: Some(archive_none()),
            mode: None,
        })
        .collect()
}

/// `save_artifact!("key")` output ports: Argo pushes the written file to
/// the exact S3 object `key` in the configured repo (raw, `archive: none`).
pub fn artifact_outputs(ctx: &BuildCtx, keys: &[String]) -> Vec<api::Artifact> {
    let s3 = &ctx.config().artifact_repository.s3;
    keys.iter()
        .map(|k| api::Artifact {
            name: artifact_ident(k),
            path: format!("{}/{k}", rt::OUT_DIR),
            s3: Some(s3_loc(s3, k)),
            archive: Some(archive_none()),
            mode: None,
        })
        .collect()
}

/// Accumulates the reachable templates (as `WorkflowTemplate`s) and the
/// run-mode dispatch table while `Template::collect` walks the closure.
pub struct Collector {
    seen: HashSet<String>,
    /// Deferred so `athena.toml` is read only at emit, never run-mode.
    builders: Vec<fn(&BuildCtx) -> api::Template>,
    runners: HashMap<String, fn(serde_json::Value) -> serde_json::Value>,
}

impl Default for Collector {
    fn default() -> Self {
        Self::new()
    }
}

impl Collector {
    pub fn new() -> Self {
        Self {
            seen: HashSet::new(),
            builders: Vec::new(),
            runners: HashMap::new(),
        }
    }

    /// Returns `false` if `argo_name` was already collected (generated
    /// `collect` impls return early in that case — dedup + cycle guard).
    pub fn enter(&mut self, argo_name: &str) -> bool {
        self.seen.insert(argo_name.to_string())
    }

    /// Register a template's `build` fn (invoked lazily at emit).
    pub fn add_builder(&mut self, build: fn(&BuildCtx) -> api::Template) {
        self.builders.push(build);
    }

    pub fn add_runner(
        &mut self,
        argo_name: &str,
        run: fn(serde_json::Value) -> serde_json::Value,
    ) {
        self.runners.insert(argo_name.to_string(), run);
    }

    /// Emit the multi-document stream: one `WorkflowTemplate` per template
    /// plus a runnable `Workflow` for the entrypoint `E`. Builds the
    /// `BuildCtx` (and reads `athena.toml`) here — emit only.
    /// Emit the multi-doc YAML. `with_workflow` appends a convenience
    /// runnable `Workflow` (`generateName`, `workflowTemplateRef` →
    /// root) — off by default: the deterministic, stable-named
    /// `WorkflowTemplate`s are the artifact you register/GitOps, and
    /// runs are triggered with `argo submit --from
    /// workflowtemplate/<root>`. The convenience Workflow is opt-in for
    /// quick demos / `kubectl create -f -`.
    pub fn emit<E: Template>(&self, with_workflow: bool) -> String {
        let ctx = BuildCtx::collect();
        let mut tpls: Vec<api::WorkflowTemplate> = self
            .builders
            .iter()
            .map(|b| {
                let inner = b(&ctx);
                wrap_workflow_template(inner.name.clone(), inner)
            })
            .collect();
        tpls.sort_by_key(name_of);

        // Whole-workflow `on_exit`: carry the exit hook on the ROOT
        // WorkflowTemplate's spec, so it fires whether the workflow is
        // run via `argo submit --from workflowtemplate/<root>` or a
        // `workflowTemplateRef` Workflow (both proven on real Argo
        // v4.0.5). `templateRef` (not the legacy `spec.onExit`
        // name-string) is what survives the one-WT-per-template model.
        if let Some(n) = E::ON_EXIT
            && let Some(root) =
                tpls.iter_mut().find(|t| name_of(t) == E::ARGO_NAME)
            && let Some(spec) = root.spec.as_mut()
        {
            spec.hooks.insert(
                "exit".to_string(),
                api::LifecycleHook {
                    template_ref: Some(api::TemplateRef {
                        name: n.to_string(),
                        template: n.to_string(),
                        cluster_scope: false,
                    }),
                    ..Default::default()
                },
            );
        }

        let mut docs: Vec<String> = tpls
            .iter()
            .map(|t| serde_norway::to_string(t).expect("WorkflowTemplate is serializable"))
            .collect();

        if !with_workflow {
            return docs.join("---\n");
        }

        let wf = api::Workflow {
            api_version: api::API_VERSION.to_string(),
            kind: api::KIND_WORKFLOW.to_string(),
            metadata: Some(api::ObjectMeta {
                generate_name: format!("{}-", E::ARGO_NAME),
                ..Default::default()
            }),
            spec: Some(api::WorkflowSpec {
                workflow_template_ref: Some(api::WorkflowTemplateRef {
                    name: E::ARGO_NAME.to_string(),
                    cluster_scope: false,
                }),
                service_account_name: ctx.config().defaults.service_account.clone(),
                // Only the emit root's on_exit reaches a runnable Workflow,
                // and it must be `hooks.exit` with a templateRef (the
                // legacy `spec.onExit` name-string can't cross the
                // one-WT-per-template wormhole).
                hooks: E::ON_EXIT
                    .map(|n| {
                        let mut m = std::collections::BTreeMap::new();
                        m.insert(
                            "exit".to_string(),
                            api::LifecycleHook {
                                template_ref: Some(api::TemplateRef {
                                    name: n.to_string(),
                                    template: n.to_string(),
                                    cluster_scope: false,
                                }),
                                ..Default::default()
                            },
                        );
                        m
                    })
                    .unwrap_or_default(),
                ..Default::default()
            }),
        };
        docs.push(serde_norway::to_string(&wf).expect("Workflow is serializable"));
        docs.join("---\n")
    }
}

fn name_of(t: &api::WorkflowTemplate) -> String {
    t.metadata.as_ref().map(|m| m.name.clone()).unwrap_or_default()
}

/// Wrap one inner Argo `template` as a standalone `WorkflowTemplate` whose
/// resource name == the inner template name == its entrypoint.
pub fn wrap_workflow_template(name: String, inner: api::Template) -> api::WorkflowTemplate {
    api::WorkflowTemplate {
        api_version: api::API_VERSION.to_string(),
        kind: api::KIND_WORKFLOW_TEMPLATE.to_string(),
        metadata: Some(api::ObjectMeta {
            name: name.clone(),
            ..Default::default()
        }),
        spec: Some(api::WorkflowSpec {
            entrypoint: name,
            templates: vec![inner],
            arguments: None,
            workflow_template_ref: None,
            ..Default::default()
        }),
    }
}

/// kebab-case an Argo identifier (DNS-1123-ish) from a Rust ident.
pub fn kebab(s: &str) -> String {
    s.replace('_', "-").to_ascii_lowercase()
}

fn volume_name(path: &str) -> String {
    let mut n: String = path
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    n = n.trim_matches('-').to_string();
    if n.is_empty() {
        n.push('v');
    }
    format!("host-{n}")
}

/// `volumes` + `volumeMounts` for a set of hostPaths (from `host!`).
pub fn host_path_volumes(paths: &[String]) -> (Vec<api::Volume>, Vec<api::VolumeMount>) {
    let mut vols = Vec::new();
    let mut mounts = Vec::new();
    for p in paths {
        let name = volume_name(p);
        vols.push(api::Volume {
            name: name.clone(),
            host_path: Some(api::HostPathVolumeSource {
                path: p.clone(),
                r#type: String::new(),
            }),
            ..Default::default()
        });
        mounts.push(api::VolumeMount {
            name,
            mount_path: p.clone(),
            read_only: false,
        });
    }
    (vols, mounts)
}

/// Every container template's volumes/mounts: the always-present
/// `emptyDir` scratch at [`ATHENA_DIR`] (binary tarball, in/out artifact
/// ports, extraction, result — all writable on any image) followed by the
/// declared hostPaths.
pub fn container_volumes(host_paths: &[String]) -> (Vec<api::Volume>, Vec<api::VolumeMount>) {
    let mut vols = vec![api::Volume {
        name: SCRATCH_VOLUME.to_string(),
        empty_dir: Some(api::EmptyDirVolumeSource {}),
        ..Default::default()
    }];
    let mut mounts = vec![api::VolumeMount {
        name: SCRATCH_VOLUME.to_string(),
        mount_path: ATHENA_DIR.to_string(),
        read_only: false,
    }];
    let (hv, hm) = host_path_volumes(host_paths);
    vols.extend(hv);
    mounts.extend(hm);
    (vols, mounts)
}

/// The entrypoint a user's `main` calls, parameterised by the root
/// workflow type. Referencing `E` force-links the entire reachable
/// closure (each `collect` calls callees' `collect` directly).
pub fn entrypoint<E: Template>() {
    let mut collector = Collector::new();
    E::collect(&mut collector);

    let args: Vec<String> = std::env::args().collect();
    let target = args
        .iter()
        .position(|a| a == "--cargo-athena-template")
        .and_then(|i| args.get(i + 1).cloned())
        .or_else(|| std::env::var("CARGO_ATHENA_TEMPLATE").ok());

    if let Some(t) = target {
        let run = *collector
            .runners
            .get(&t)
            .unwrap_or_else(|| panic!("no runnable container template named {t:?}"));
        // Base inputs: `cargo athena run --input` (local), then overlay
        // Argo-supplied params delivered as `ATHENA_PARAM_<name>` env vars
        // (set on the container template from `{{inputs.parameters.*}}`).
        let mut input = std::env::var("CARGO_ATHENA_INPUT")
            .ok()
            .map(|s| serde_json::from_str(&s).expect("CARGO_ATHENA_INPUT must be JSON"))
            .unwrap_or(serde_json::Value::Object(Default::default()));
        if let serde_json::Value::Object(map) = &mut input {
            for (k, v) in std::env::vars() {
                if let Some(name) = k.strip_prefix("ATHENA_PARAM_") {
                    // A param is JSON if it parses, else a bare string.
                    let val = serde_json::from_str(&v)
                        .unwrap_or(serde_json::Value::String(v.clone()));
                    map.insert(name.to_string(), val);
                }
            }
        }
        let output = run(input);
        if let Ok(path) = std::env::var("CARGO_ATHENA_OUTPUT") {
            std::fs::write(path, output.to_string()).expect("write CARGO_ATHENA_OUTPUT");
        } else {
            println!("{output}");
        }
        return;
    }

    // `cargo athena emit --with-workflow` sets this on the child so the
    // convenience runnable Workflow is appended (default: templates
    // only — deterministic, `kubectl apply`-able, GitOps-clean).
    let with_workflow = std::env::var_os("CARGO_ATHENA_WITH_WORKFLOW")
        .is_some_and(|v| v == "1");
    print!("{}", collector.emit::<E>(with_workflow));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kebab_lowercases_and_hyphenates() {
        assert_eq!(kebab("run_a_container"), "run-a-container");
        assert_eq!(kebab("RunFoo"), "runfoo");
    }

    #[test]
    fn volume_name_is_dns_safe() {
        assert_eq!(volume_name("/etc/myapp"), "host-etc-myapp");
        assert_eq!(volume_name("/var/lib/extra"), "host-var-lib-extra");
    }
}
