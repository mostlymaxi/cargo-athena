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
pub use serde_yaml;

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

/// Runtime shims referenced by the declaration macros.
pub mod rt {
    /// Identity: the volume is already mounted when the container runs.
    pub const fn host_path(path: &'static str) -> &'static str {
        path
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

/// Where the binary tarball is mounted inside the pod.
pub const ARTIFACT_PATH: &str = "/athena/dist.tar.gz";

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

    let script = format!(
        "set -e\n\
         case \"$(uname -m)\" in\n\
         {arms}  *) echo \"athena: unsupported arch $(uname -m)\" >&2; exit 1 ;;\n\
         esac\n\
         __d=$(mktemp -d)\n\
         tar -xzf {ARTIFACT_PATH} -C \"$__d\"\n\
         chmod +x \"$__d/app-$__t\"\n\
         exec \"$__d/app-$__t\" --cargo-athena-template {argo_name}\n"
    );

    let artifact = api::Artifact {
        name: "athena-dist".to_string(),
        path: ARTIFACT_PATH.to_string(),
        s3: Some(api::S3Artifact {
            endpoint: s3.endpoint.clone(),
            bucket: s3.bucket.clone(),
            region: s3.region.clone(),
            insecure: s3.insecure,
            key: cfg.artifact.key.clone(),
            access_key_secret: Some(api::SecretKeySelector {
                name: s3.access_key_secret.name.clone(),
                key: s3.access_key_secret.key.clone(),
            }),
            secret_key_secret: Some(api::SecretKeySelector {
                name: s3.secret_key_secret.name.clone(),
                key: s3.secret_key_secret.key.clone(),
            }),
        }),
        archive: Some(api::ArchiveStrategy {
            none: Some(api::NoneStrategy {}),
        }),
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

    /// A container's own literal `host!` paths ∪ the transitive
    /// `#[fragment]` closure (deduped, stable order).
    pub fn resolved_host_paths(&self, own_host: &[&str], own_callees: &[&str]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        let push = |p: &str, out: &mut Vec<String>, seen: &mut HashSet<String>| {
            if seen.insert(p.to_string()) {
                out.push(p.to_string());
            }
        };
        for p in own_host {
            push(p, &mut out, &mut seen);
        }
        let mut queue: Vec<&str> = own_callees.to_vec();
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(c) = queue.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(f) = self.fragments.get(c) {
                for p in f.host_paths {
                    push(p, &mut out, &mut seen);
                }
                queue.extend(f.callees.iter().copied());
            }
        }
        out
    }
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
    pub fn emit<E: Template>(&self) -> String {
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

        let mut docs: Vec<String> = tpls
            .iter()
            .map(|t| serde_yaml::to_string(t).expect("WorkflowTemplate is serializable"))
            .collect();

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
                ..Default::default()
            }),
        };
        docs.push(serde_yaml::to_string(&wf).expect("Workflow is serializable"));
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

/// Build the `volumes` + matching `volumeMounts` for a set of hostPaths.
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
        });
        mounts.push(api::VolumeMount {
            name,
            mount_path: p.clone(),
            read_only: false,
        });
    }
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
        let input: serde_json::Value = std::env::var("CARGO_ATHENA_INPUT")
            .ok()
            .map(|s| serde_json::from_str(&s).expect("CARGO_ATHENA_INPUT must be JSON"))
            .unwrap_or(serde_json::Value::Object(Default::default()));
        let output = run(input);
        if let Ok(path) = std::env::var("CARGO_ATHENA_OUTPUT") {
            std::fs::write(path, output.to_string()).expect("write CARGO_ATHENA_OUTPUT");
        } else {
            println!("{output}");
        }
        return;
    }

    print!("{}", collector.emit::<E>());
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
