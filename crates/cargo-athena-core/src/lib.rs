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
//! * **Emit**: `main` calls `cargo_athena::entrypoint!(E)` (which calls
//!   [`entrypoint_impl::<E>()`]); we walk the closure
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
    String, str, i8, i16, i32, i64, i128, isize, u8, u16, u32, u64, u128, usize, f32, f64,
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
    ($name:literal) => {
        $crate::rt::load_artifact($name)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("load_artifact!(\"name\")")
    };
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
    ($name:literal) => {
        $crate::rt::load_artifact_str($name)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("load_artifact_str!(\"name\")")
    };
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
    ($name:literal, $data:expr) => {
        $crate::rt::save_artifact($name, $data)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("save_artifact!(\"name\", data)")
    };
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
    ($name:literal, $data:expr) => {
        $crate::rt::save_artifact_str($name, $data)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("save_artifact_str!(\"name\", data)")
    };
}

/// Declare a K8s Secret-sourced env var and read it back at runtime as
/// a `String`. Public form: `compile_error!` outside
/// `#[container]`/`#[fragment]` (same gating as `host!`). Two literal
/// args: `(secret_name, key)`. Panics at runtime if the env var the
/// macro plants isn't set; pair with `secret_opt!` for the no-panic
/// variant.
#[macro_export]
macro_rules! secret {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`secret!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_secret {
    ($name:literal, $key:literal) => {
        $crate::rt::secret_value($name, $key)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("secret!(\"secret-name\", \"key\")")
    };
}

/// Same as [`secret!`] but returns `Option<String>` and emits
/// `optional: true` on the Argo `secretKeyRef` — Argo skips the env
/// entry instead of failing pod-start when the secret/key is missing.
#[macro_export]
macro_rules! secret_opt {
    ($($t:tt)*) => {
        ::core::compile_error!(
            "`secret_opt!` may only be used directly inside a \
             `#[cargo_athena::container]` or `#[cargo_athena::fragment]` fn"
        )
    };
}
#[doc(hidden)]
#[macro_export]
macro_rules! __cargo_athena_secret_opt {
    ($name:literal, $key:literal) => {
        $crate::rt::secret_value_opt($name, $key)
    };
    ($($t:tt)*) => {
        ::core::compile_error!("secret_opt!(\"secret-name\", \"key\")")
    };
}

/// Runtime shims referenced by the declaration macros. Artifact ports are
/// plain files at fixed paths; Argo moves them (no S3 from us).
pub mod rt {
    use std::path::PathBuf;

    /// Resolve a `host!("/p")` to its in-pod mount path. We deliberately
    /// **do not** mount host paths at the same in-container path —
    /// `host!("/")` would otherwise overlay-mount the host root over
    /// the container's root, and `host!("/etc")` would shadow the
    /// image's own `/etc`. Instead the hostPath is mounted under
    /// `[/athena/mounts/<munged>]` and the macro returns that path, so
    /// user code stays portable and can't accidentally clobber the
    /// image's filesystem. Both emit and run sides go through
    /// [`super::host_mount_path`] so they agree.
    pub fn host_path(host: &str) -> String {
        super::host_mount_path(host)
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

    /// `secret!(name, key)` runtime: read the env var the macro emits.
    /// Panics if missing (use [`secret_value_opt`] for the no-panic
    /// flavour, paired with `secret_opt!` on the declaration side).
    pub fn secret_value(name: &str, key: &str) -> String {
        let var = super::secret_env_name(name, key);
        std::env::var(&var).unwrap_or_else(|e| {
            panic!("athena: secret!({name:?}, {key:?}) env var `{var}` missing: {e}")
        })
    }

    /// `secret_opt!(name, key)` runtime: `None` when the env var is
    /// unset (e.g. Argo skipped the entry because the secret/key is
    /// missing and `optional: true`).
    pub fn secret_value_opt(name: &str, key: &str) -> Option<String> {
        std::env::var(super::secret_env_name(name, key)).ok()
    }
}

/// The pod env var name a `secret!`/`secret_opt!` decl gets, derived
/// deterministically from the K8s `(secret_name, key)` pair so the
/// emit-side and the run-side agree. Uppercased, non-alphanumerics
/// flattened to `_`, separated by `__` so the two halves stay
/// distinguishable. Both sides go through this function — never
/// hard-code an env name elsewhere.
pub fn secret_env_name(name: &str, key: &str) -> String {
    let mut s = String::from("ATHENA_SEC_");
    push_munged(&mut s, name);
    s.push_str("__");
    push_munged(&mut s, key);
    s
}

fn push_munged(out: &mut String, input: &str) {
    for c in input.chars() {
        out.push(if c.is_ascii_alphanumeric() {
            c.to_ascii_uppercase()
        } else {
            '_'
        });
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

/// How a single Argo input or output flows: inline as a `parameter` (Argo
/// stores it in workflow status, sized like a JSON parameter) or via S3
/// as an `artifact` (DAG-wired via `outputs.artifacts` /
/// `arguments.artifacts.from`). The `#[container]` and `#[workflow]`
/// macros derive these per-slot from the function signature: a fn
/// argument or return type of `cargo_athena::Artifact<T>` is `Artifact`,
/// everything else is `Parameter`. Stamped into
/// [`Template::INPUT_KINDS`] / [`Template::OUTPUT_KIND`] and read at
/// emit-time by the workflow's per-task wiring (parallel to how
/// [`Template::INPUTS`] is read for parameter names).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum IoKind {
    Parameter,
    Artifact,
}

/// A DAG-wired S3-backed value. Wrap a `#[container]` (or `#[workflow]`)
/// return in `Artifact<T>` to flow the value via Argo's native artifact
/// passing (`outputs.artifacts.return` + `arguments.artifacts.from`)
/// instead of the inline-parameter path (`outputs.parameters.return`).
/// Lifts the parameter-size ceiling and is the natural shape for
/// binary/large payloads. A consumer accepting `Artifact<T>` reads the
/// same value on the other side; the wire is the user's serialized `T`
/// (JSON, tar+gzip'd by Argo on the producer and untarred on the
/// consumer, transparent to user code).
///
/// The inner `T` is private by design: no field-access (`a.field`) on
/// an `Artifact<T>` binding, no `Deref<Target=T>`, no `AthenaList<_>`
/// impl. Those constraints fall out of Rust's own type rules in the
/// `#[workflow]` ghost (see `feedback-ghost-first.md` in agent
/// memory) — they are NOT enforced by bespoke macro checks. The only
/// public surface is `Artifact::new` / `Artifact::into_inner`.
pub struct Artifact<T> {
    inner: T,
    _marker: ::core::marker::PhantomData<fn() -> T>,
}

impl<T> Artifact<T> {
    pub fn new(v: T) -> Self {
        Self {
            inner: v,
            _marker: ::core::marker::PhantomData,
        }
    }

    pub fn into_inner(self) -> T {
        self.inner
    }
}

// `.clone()` in a `#[workflow]` body is the fan-out marker that lets
// the same producer feed multiple consumers (see WORKFLOW.md). The
// ghost type-checks it as a real `Clone` call so the binding has to
// implement `Clone`; the macro lowers it as "reference the same
// upstream task again", so no runtime clone happens in-pod.
impl<T: Clone> Clone for Artifact<T> {
    fn clone(&self) -> Self {
        Self::new(self.inner.clone())
    }
}

// Serde-transparent: the wire form is plain serialized `T`. The wrapper
// is purely a Rust type marker that the macros key on; on disk it is
// indistinguishable from `T` itself. Lets `#[container]`'s `run()` body
// write `serde_json::to_writer(File::create(...), &val)` after
// `.into_inner()` without any wrapper bytes leaking through.
impl<T: ::serde::Serialize> ::serde::Serialize for Artifact<T> {
    fn serialize<S: ::serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(s)
    }
}

impl<'de, T: ::serde::Deserialize<'de>> ::serde::Deserialize<'de> for Artifact<T> {
    fn deserialize<D: ::serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        T::deserialize(d).map(Self::new)
    }
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
    /// Stringified Rust types of [`Self::INPUTS`], same order — for
    /// `container emulate`'s pre-launch arg checking and the `ls`
    /// listings. Emitted by `#[container]` and `#[workflow]`; defaulted
    /// empty for synthetic/hand impls.
    const INPUT_TYPES: &'static [&'static str] = &[];
    /// Per-input I/O kind, parallel to [`Self::INPUTS`]. An empty slice
    /// (the backwards-compat default) means "all parameters" — what
    /// every template did before [`Artifact`] landed. The
    /// `#[container]` / `#[workflow]` macros set entries to
    /// [`IoKind::Artifact`] for any argument typed `Artifact<T>` and
    /// [`IoKind::Parameter`] for everything else. The workflow's
    /// per-task wiring reads this at emit-time to decide between
    /// `arguments.parameters[].value` and `arguments.artifacts[].from`.
    const INPUT_KINDS: &'static [IoKind] = &[];
    /// I/O kind of the function's return value. Default
    /// [`IoKind::Parameter`] keeps every existing template's
    /// `outputs.parameters.return` emission byte-identical; macros set
    /// this to [`IoKind::Artifact`] when the return type is
    /// `Artifact<T>` so the template emits `outputs.artifacts.return`
    /// (S3-backed) instead.
    const OUTPUT_KIND: IoKind = IoKind::Parameter;
    /// `true` for athena-synthesized templates (the `if`/`else`
    /// wrapper + per-arm sub-workflows). They're an implementation
    /// detail, so `workflow ls` hides them unless `--include-synthetic`.
    const SYNTHETIC: bool = false;
    const KIND: TemplateKind;
    /// Whole-workflow exit-handler template name, from
    /// `#[workflow(on_exit_if_root=…)]` / `#[container(on_exit_if_root=…)]`.
    /// `emit` puts it on this template's own `spec.hooks.exit`; Argo
    /// fires exit hooks workflow-scoped, so it runs only when this
    /// workflow is the one submitted (inert as a nested templateRef).
    const ON_EXIT: Option<&'static str> = None;
    /// Workflow-scoped TTL GC, from `#[…(ttl(..))]`. `build_templates`
    /// puts it on this template's own `spec.ttlStrategy` (same per-WT
    /// plumbing as `ON_EXIT`; never on synthetic `if` wrappers).
    const TTL: ::core::option::Option<crate::api::TtlStrategy> = None;
    /// Workflow-scoped pod GC strategy, from `#[…(pod_gc(strategy=..))]`.
    /// `build_templates` puts it on this template's own `spec.podGC`.
    const POD_GC: ::core::option::Option<&'static str> = None;
    /// Root-only whole-workflow runtime cap (seconds), from
    /// `#[…(active_deadline_if_root=..)]`. `build_templates` stamps it
    /// on this template's own `spec.activeDeadlineSeconds` (same per-WT,
    /// root-only plumbing as `TTL`/`POD_GC`). This is the *only* working
    /// whole-workflow timeout: Argo applies neither `Template.timeout`
    /// nor `Template.activeDeadlineSeconds` to dag/steps templates.
    const ACTIVE_DEADLINE_IF_ROOT: ::core::option::Option<i64> = None;
    /// Root-only `WorkflowSpec.nodeSelector`, from
    /// `#[workflow(node_selector_if_root = { … })]`. Argo's pod-build
    /// lookup is 3-tier: `tmpl.NodeSelector → boundary.NodeSelector →
    /// wfSpec.NodeSelector`, so this is the only knob that lands on
    /// EVERY pod in the run unless that pod (or its immediate enclosing
    /// dag/steps) overrides it (verified from `workflow/controller/
    /// workflowpod.go:928-958`). Same per-WT plumbing as
    /// `TTL`/`POD_GC`/`ACTIVE_DEADLINE_IF_ROOT`. Literal pairs only —
    /// workflow attrs have no injectable args (see `#[workflow(node
    /// _selector)]` for the rationale).
    const NODE_SELECTOR_IF_ROOT: &'static [(&'static str, &'static str)] = &[];
    /// Root-only `WorkflowSpec.synchronization.mutexes`, from
    /// `#[…(mutexes_if_root = [{ name = …, namespace = … }])]`. Each
    /// entry is `(name, namespace)`, already lowered to its final YAML
    /// string form (literal, or `{{=fromJSON(workflow.parameters[…])}}`
    /// for injected operands). `namespace == ""` ⇒ skip the field
    /// (Argo defaults to the workflow's own namespace per
    /// `workflow/sync/lock_name.go:58-67`). Same per-WT, root-only
    /// plumbing as `TTL` / `POD_GC` / `NODE_SELECTOR_IF_ROOT`; Argo's
    /// sync manager keys on `<ns>/Mutex/<name>` globally so two
    /// SEPARATE Workflow runs contend on the same name (empirically
    /// verified on v4.0.5 2026-05-25, holder key `<ns>/<wf>`).
    const MUTEXES_IF_ROOT: &'static [(&'static str, &'static str)] = &[];
    /// Root-only `WorkflowSpec.Tolerations` from
    /// `#[…(tolerations_if_root = [...])]`. Each entry is `(key,
    /// operator, value, effect, toleration_seconds)`. Strings are
    /// already lowered (literal verbatim, or
    /// `{{=fromJSON(workflow.parameters[..])}}` for injected operands).
    /// `toleration_seconds == 0` ⇒ Argo skip-serializes (treated as
    /// "unset" by k8s, applies forever).
    const TOLERATIONS_IF_ROOT: &'static [(
        &'static str,
        &'static str,
        &'static str,
        &'static str,
        i64,
    )] = &[];
    /// Root-only `WorkflowSpec.Affinity` from
    /// `#[…(affinity_if_root = "...")]` as an opaque YAML/JSON string.
    /// Parsed at emit time; user owns the schema (athena does NOT
    /// validate). None ⇒ skip.
    const AFFINITY_IF_ROOT: ::core::option::Option<&'static str> = None;
    /// Root-only `WorkflowSpec.PodSpecPatch`, from
    /// `#[workflow(pod_spec_patch_if_root = "...")]`. Already lowered
    /// to its final string form (literal verbatim, or with
    /// `{{=fromJSON(workflow.parameters[..])}}` operands spliced in
    /// for injected pieces). Same per-WT, root-only plumbing as
    /// `NODE_SELECTOR_IF_ROOT`. None ⇒ skip (existing goldens
    /// unaffected).
    const POD_SPEC_PATCH_IF_ROOT: ::core::option::Option<&'static str> = None;
    /// Root-only `WorkflowSpec.ImagePullSecrets` (Secret names) from
    /// `#[…(image_pull_secrets_if_root = ["regcred", ...])]`. K8s/
    /// Argo expose this only at workflow scope (no per-template
    /// knob); per-container needs go through `pod_spec_patch`. Same
    /// per-WT, root-only plumbing as `MUTEXES_IF_ROOT`.
    const IMAGE_PULL_SECRETS_IF_ROOT: &'static [&'static str] = &[];

    /// Build this template's inner Argo `template` object.
    fn build(ctx: &BuildCtx) -> api::Template;

    /// Run-mode body. Argv is the function's positional parameters in
    /// `INPUTS` order, each JSON-encoded (string -> `"v"`, number/bool
    /// bare). Returns the function's JSON-encoded return value, which
    /// the entrypoint writes to `CARGO_ATHENA_OUTPUT`. Overridden by
    /// `#[container]`; never called on a `#[workflow]`.
    fn run(_argv: &[String]) -> String {
        panic!(
            "`{}` is not a #[container]; nothing to run",
            Self::ARGO_NAME
        )
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
    /// Default cargo package the `cargo athena` subcommands drive (so
    /// you don't repeat `--package`). The `--package`/`-p` flag wins.
    #[serde(default)]
    pub package: Option<String>,
    /// Default cargo bin within that package (multi-bin crates need
    /// it). The `--bin` flag wins.
    #[serde(default)]
    pub bin: Option<String>,
    /// Default Kubernetes namespace for `cargo athena submit`. Precedence:
    /// `-n/--namespace` → `$ARGO_NAMESPACE` → this → `"default"`.
    #[serde(default)]
    pub namespace: Option<String>,
}

impl Default for Defaults {
    fn default() -> Self {
        Self {
            service_account: default_service_account(),
            package: None,
            bin: None,
            namespace: None,
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
/// Where Argo's executor (init container) extracts the per-arch
/// binaries from our `.tar.gz` input artifact. We rely on Argo's
/// built-in tarball auto-extraction (no `archive: none`, no `tar` in
/// the main container's image — see `container_delivery`).
pub const ATHENA_BIN_DIR: &str = "/athena/bin";
/// Where every `host!`-declared hostPath gets mounted inside the
/// container. Mounting at the host's own path (e.g. `host!("/")` →
/// `/`) would overlay-mount the host filesystem on top of the image
/// — a footgun and a security risk. Routing through this directory
/// makes `host!` safe-by-construction; the `host_mount` `#[container]`
/// attr is the explicit escape hatch for same-path / chosen-path
/// mounts.
pub const ATHENA_MOUNTS_DIR: &str = "/athena/mounts";
/// Where a *parameter-output* `#[container]` body writes its serialized
/// return value (read by the template's
/// `outputs.parameters.return.valueFrom.path`). The bootstrap exports
/// this as `CARGO_ATHENA_OUTPUT` for parameter-output templates.
pub const ATHENA_RESULT_FILE: &str = "/athena/result";
/// Where an *artifact-output* `#[container]` body writes its serialized
/// return value (read by the template's `outputs.artifacts.return.path`,
/// then tar+gzip'd and uploaded by Argo's executor). The bootstrap
/// exports this as `CARGO_ATHENA_OUTPUT` for artifact-output templates;
/// the run-side body has one write site (`CARGO_ATHENA_OUTPUT`) and
/// stays kind-agnostic.
pub const ATHENA_RESULT_ARTIFACT_FILE: &str = "/athena/result-artifact";
/// Where an `Artifact<T>`-typed input lands inside the container, one
/// file per arg, named after the arg. The template declares
/// `inputs.artifacts[].path = "/athena/in/<name>"`; the run-side body
/// reads + deserializes from there.
pub const ATHENA_INPUT_ARTIFACT_DIR: &str = "/athena/in";
/// The in-pod arch-resolving + exec bootstrap, kept in a separate
/// `bootstrap.sh` so it can be read, edited, and `shellcheck`'d as a
/// plain shell file rather than buried in a Rust `format!`. `@@ARMS@@`
/// / `@@BIN_DIR@@` / `@@OUTPUT_PATH@@` are substituted at emit time in
/// `container_delivery`.
const BOOTSTRAP_TEMPLATE: &str = include_str!("bootstrap.sh");
/// Name of the scratch `emptyDir` volume.
pub const SCRATCH_VOLUME: &str = "athena-work";
/// Argo input-artifact name of the binary tarball `emit` injects.
pub const ATHENA_DIST_ARTIFACT: &str = "athena-dist";
/// Env var the in-pod entrypoint reads to pick which container template
/// to run. Argv (positional, in `INPUTS` order) carries the function's
/// own parameters.
pub const CARGO_ATHENA_TEMPLATE_ENV: &str = "CARGO_ATHENA_TEMPLATE";

/// `true` if this binary is dispatching a container body (in-pod,
/// started by Argo via the emitted bootstrap); `false` for every other
/// mode (`cargo athena emit` / `ls` / `describe` / `submit`-emit-JSON).
///
/// Use this in `main()` to gate one-time setup you only want to fire
/// in-pod -- a tracing/OTLP subscriber, a metrics exporter, anything
/// that costs network or has side effects. Without the gate, those
/// would also fire on every local `cargo athena emit` etc. that spawns
/// the binary to introspect templates.
///
/// ```ignore
/// fn main() {
///     let _otel = cargo_athena::is_container_run().then(|| {
///         tracing_subscriber::fmt().init();
///         OtelFlushGuard::new()        // drops at end of main()
///     });
///     cargo_athena::entrypoint!(MyRoot);
/// }
/// ```
pub fn is_container_run() -> bool {
    std::env::var_os(CARGO_ATHENA_TEMPLATE_ENV).is_some()
}

/// Resolved S3 coordinates for one artifact (creds are supplied
/// locally, e.g. via AWS env vars — `cargo athena container run` uses
/// `object_store`; the in-cluster path uses the k8s Secret refs).
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct S3Ref {
    pub endpoint: String,
    pub bucket: String,
    pub region: String,
    pub insecure: bool,
    pub key: String,
}

/// One artifact bound into the container at `path`, backed by S3.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ArtifactRef {
    pub s3: S3Ref,
    pub path: String,
}

/// An input parameter and its stringified Rust type (`""` if unknown,
/// e.g. synthetic templates). The position in the enclosing `params`
/// vector is the positional argv slot the parameter occupies in-pod.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ParamRef {
    pub name: String,
    pub ty: String,
}

/// Purpose-built introspection of one `#[container]`, derived from the
/// *same* `Template::build()` `emit` uses (so it never drifts), but
/// expressed in the runner's vocabulary instead of Argo's. Emitted as
/// JSON by the binary when `CARGO_ATHENA_DESCRIBE=<name>` is set;
/// consumed by `cargo athena container run` to realize the spec under
/// docker/podman locally. Also the basis for a future
/// `cargo athena container describe`.
#[derive(serde::Serialize, serde::Deserialize, Debug, Clone)]
pub struct ContainerRunMeta {
    /// Argo template name (`<crate>-<fn>`).
    pub name: String,
    /// Source crate (CARGO_PKG_NAME of the user binary). Used for the
    /// `PACKAGE` column in `container ls` / `workflow ls`; an empty
    /// string means the caller didn't fill it in.
    #[serde(default)]
    pub package: String,
    /// `"container"`, `"workflow"`, or `"other"`.
    pub kind: String,
    /// athena-synthesized template (an `if`/`else` wrapper or arm) —
    /// `workflow ls` hides these unless `--include-synthetic`.
    pub synthetic: bool,
    /// Resolved container image.
    pub image: String,
    /// The injected bootstrap command + args, verbatim — run as-is so
    /// the local execution path is byte-identical to the pod's.
    pub command: Vec<String>,
    pub args: Vec<String>,
    /// Mount path of the pod-scoped scratch dir (the `emptyDir`, e.g.
    /// `/athena`); bind a host temp dir here to read `result_path` back.
    pub work_dir: String,
    /// Input parameters and the env var each is delivered through.
    pub params: Vec<ParamRef>,
    /// The binary tarball artifact (always present for a container).
    pub binary_artifact: Option<ArtifactRef>,
    /// `load_artifact!` input ports (excludes the binary tarball).
    pub input_artifacts: Vec<ArtifactRef>,
    /// `save_artifact!` output ports.
    pub output_artifacts: Vec<ArtifactRef>,
    /// `host!` paths (mounted at the same path in-pod; bind 1:1 locally).
    pub host_paths: Vec<String>,
    /// File the body writes its serialized return to
    /// (`outputs.parameters.return`); read it back from the bind mount.
    pub result_path: Option<String>,
}

/// Drop the spaces `quote!` puts around `<` / `>` / `,` so a type
/// like `Vec < String >` round-trips as `Vec<String>` for human
/// display. `validate_args` already strips ALL whitespace; this is the
/// gentler variant for showing types verbatim (`Cow<'static, str>`
/// stays readable).
fn normalize_ty(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut prev_open = false;
    for ch in s.chars() {
        match ch {
            '<' | '>' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(ch);
                prev_open = ch == '<';
            }
            ',' => {
                while out.ends_with(' ') {
                    out.pop();
                }
                out.push(ch);
                prev_open = false;
            }
            ' ' if prev_open => {}
            ' ' if out.ends_with('<') || out.ends_with(',') => {}
            _ => {
                out.push(ch);
                prev_open = false;
            }
        }
    }
    out
}

impl ContainerRunMeta {
    /// Derive the runner metadata from one built Argo template.
    /// `input_types` is parallel to the template's input parameters
    /// (same order); empty when unknown.
    fn from_template(t: &api::Template, input_types: &[&str]) -> Self {
        let kind = if t.container.is_some() {
            "container"
        } else if t.dag.is_some() || !t.steps.is_empty() {
            "workflow"
        } else {
            "other"
        };
        let c = t.container.as_ref();
        let mount_path = |vol: &str| {
            c.and_then(|c| {
                c.volume_mounts
                    .iter()
                    .find(|m| m.name == vol)
                    .map(|m| m.mount_path.clone())
            })
        };
        let to_ref = |a: &api::Artifact| {
            a.s3.as_ref().map(|s| ArtifactRef {
                s3: S3Ref {
                    endpoint: s.endpoint.clone(),
                    bucket: s.bucket.clone(),
                    region: s.region.clone(),
                    insecure: s.insecure,
                    key: s.key.clone(),
                },
                path: a.path.clone(),
            })
        };
        let in_arts = t.inputs.as_ref().map(|i| &i.artifacts);
        ContainerRunMeta {
            name: t.name.clone(),
            // Filled in by the caller from BuildCtx's artifact_key
            // (encoded as `{crate}/{version}/{bin}.tar.gz`).
            package: String::new(),
            kind: kind.to_string(),
            // set by the caller from the Collector (Template::SYNTHETIC
            // isn't visible through the type-erased builder fn here).
            synthetic: false,
            image: c.map(|c| c.image.clone()).unwrap_or_default(),
            command: c.map(|c| c.command.clone()).unwrap_or_default(),
            args: c.map(|c| c.args.clone()).unwrap_or_default(),
            work_dir: mount_path(SCRATCH_VOLUME).unwrap_or_else(|| ATHENA_DIR.to_string()),
            params: t
                .inputs
                .as_ref()
                .map(|i| {
                    i.parameters
                        .iter()
                        .enumerate()
                        .map(|(idx, p)| ParamRef {
                            name: p.name.clone(),
                            // `quote!(#ty).to_string()` spaces `<` and
                            // `>`, e.g. `Vec < String >`. Strip those
                            // (only inside generics) for display.
                            ty: input_types
                                .get(idx)
                                .map(|s| normalize_ty(s))
                                .unwrap_or_default(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
            binary_artifact: in_arts
                .and_then(|a| a.iter().find(|a| a.name == ATHENA_DIST_ARTIFACT))
                .and_then(to_ref),
            input_artifacts: in_arts
                .map(|a| {
                    a.iter()
                        .filter(|a| a.name != ATHENA_DIST_ARTIFACT)
                        .filter_map(to_ref)
                        .collect()
                })
                .unwrap_or_default(),
            output_artifacts: t
                .outputs
                .as_ref()
                .map(|o| o.artifacts.iter().filter_map(to_ref).collect())
                .unwrap_or_default(),
            host_paths: t
                .volumes
                .iter()
                .filter_map(|v| v.host_path.as_ref().map(|h| h.path.clone()))
                .collect(),
            result_path: t.outputs.as_ref().and_then(|o| {
                o.parameters
                    .iter()
                    .find(|p| p.name == "return")
                    .and_then(|p| p.value_from.as_ref())
                    .map(|vf| vf.path.clone())
                    .filter(|p| !p.is_empty())
            }),
        }
    }
}

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
/// needs a POSIX `sh` and `uname` — **no `tar`**). The binary `.tar.gz`
/// is delivered as an Argo input artifact at [`ATHENA_BIN_DIR`]; with
/// the default `Archive` (no `archive: none`) Argo's executor init
/// container **auto-detects tarballs and untars them into the artifact
/// path** (proven from `workflow/executor/executor.go:262–289`,
/// `untar` ibid., real v4.0.5 source). So by the time our bootstrap
/// runs the per-arch `app-<triple>` files already exist under
/// [`ATHENA_BIN_DIR`]; the script just `uname`s, `chmod +x`'s, and
/// `exec`s — replacing the shell so the Rust binary is the container's
/// main process. Works whether the tarball has one entry or many.
///
/// The bootstrap forwards `"$@"` to the binary; the macro-generated
/// `Template::build` appends `--` then one `{{inputs.parameters.<n>}}`
/// per parameter as positional argv. The binary's runner reads them
/// positionally in `INPUTS` order. (We used to deliver each parameter
/// via an `ATHENA_PARAM_<n>` env var, but env vars are NOT eligible
/// for Argo's automatic large-args offload to a ConfigMap, so any
/// large parameter value would balloon the pod spec and could exceed
/// the `E2BIG` exec limit. Argo offloads `container.args` once the
/// total exceeds 128 KB.)
pub fn container_delivery(
    ctx: &BuildCtx,
    param_names: &[&str],
    image_override: Option<&str>,
    output_kind: IoKind,
) -> ContainerDelivery {
    let cfg = ctx.config();
    let s3 = &cfg.artifact_repository.s3;
    let image = image_override
        .map(str::to_string)
        .unwrap_or_else(|| cfg.bootstrap.default_image.clone());

    let mut arms = String::new();
    for triple in &cfg.bootstrap.targets {
        let arch = triple.split('-').next().unwrap_or(triple);
        let pat = if arch == "aarch64" {
            "aarch64|arm64"
        } else {
            arch
        };
        arms.push_str(&format!("  {pat}) __t={triple} ;;\n"));
    }

    // The body's serialized return value lands at `CARGO_ATHENA_OUTPUT`.
    // Parameter-output containers route it to ATHENA_RESULT_FILE (which
    // the template's `outputs.parameters.return.valueFrom.path` reads).
    // Artifact-output containers route it to ATHENA_RESULT_ARTIFACT_FILE
    // (which the template's `outputs.artifacts.return.path` reads;
    // Argo's executor then tar+gzips and uploads to S3). The bootstrap
    // exports the right path before exec-ing the binary; the run-side
    // body has one write site (`CARGO_ATHENA_OUTPUT`) and stays kind-
    // agnostic.
    let output_path = match output_kind {
        IoKind::Parameter => ATHENA_RESULT_FILE,
        IoKind::Artifact => ATHENA_RESULT_ARTIFACT_FILE,
    };

    // Argo's executor (init container) auto-extracts the `.tar.gz` into
    // ATHENA_BIN_DIR, so the bootstrap just picks + execs. The template
    // lives in a sibling `bootstrap.sh` file (legible / greppable /
    // shellcheck-able); we just substitute the @@-delimited slots.
    let script = BOOTSTRAP_TEMPLATE
        .replace("@@ARMS@@", &arms)
        .replace("@@BIN_DIR@@", ATHENA_BIN_DIR)
        .replace("@@OUTPUT_PATH@@", output_path);

    // `sh -c "<script>" -- arg1 arg2 ...` puts "--" in $0 (placeholder)
    // and arg1/arg2 in "$@", which the bootstrap forwards to the binary.
    let mut args = vec![script, "--".to_string()];
    for name in param_names {
        args.push(format!("{{{{inputs.parameters.{name}}}}}"));
    }

    // `archive: None` (NOT `archive: none`) lets Argo auto-detect the
    // input as a tarball and untar it into `path` (`ATHENA_BIN_DIR`).
    let artifact = api::Artifact {
        name: "athena-dist".to_string(),
        path: ATHENA_BIN_DIR.to_string(),
        s3: Some(s3_loc(s3, ctx.artifact_key())),
        archive: None,
        mode: None,
        from: String::new(),
    };

    ContainerDelivery {
        image,
        command: vec!["/bin/sh".to_string(), "-c".to_string()],
        args,
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
    /// `(secret_name, key, optional)` triples from this fragment's
    /// `secret!`/`secret_opt!` declarations.
    pub secrets: &'static [(&'static str, &'static str, bool)],
    pub callees: &'static [&'static str],
}
inventory::collect!(FragmentReg);

/// Fragment registry snapshot, passed to container `build`s.
pub struct BuildCtx {
    fragments: HashMap<&'static str, &'static FragmentReg>,
    config: AthenaConfig,
    /// Fully-resolved S3 object key for this binary's tarball
    /// (`{crate}/{version}/{bin}.tar.gz`). Built once by
    /// [`entrypoint_impl`] from the user binary's own
    /// `CARGO_PKG_*` / `CARGO_BIN_NAME` env
    /// vars (captured at the bin's compile time by the `entrypoint!`
    /// macro). `cargo athena publish` derives the same key from
    /// `cargo metadata`, so the upload and the emitted YAML always
    /// agree by construction.
    artifact_key: String,
}

impl BuildCtx {
    /// Emit-only: gathers fragments AND loads `athena.toml`. Never called
    /// in run-mode, so the in-pod binary needs no `athena.toml`.
    pub fn collect(krate: &str, version: &str, bin: &str) -> Self {
        let mut fragments = HashMap::new();
        for f in inventory::iter::<FragmentReg> {
            fragments.insert(f.rust_name, f);
        }
        Self {
            fragments,
            config: AthenaConfig::load(),
            artifact_key: format!("{krate}/{version}/{bin}.tar.gz"),
        }
    }

    pub fn config(&self) -> &AthenaConfig {
        &self.config
    }

    /// The S3 object key of this binary's tarball, hardcoded as
    /// `{crate}/{version}/{bin}.tar.gz`.
    pub fn artifact_key(&self) -> &str {
        &self.artifact_key
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

    /// Env-var-sourced K8s secrets: own `secret!`/`secret_opt!` decls
    /// ∪ the `#[fragment]` closure (deduped on the `(name, key)` pair —
    /// optionality is preserved from the first occurrence). Same shape
    /// as `resolved`, but the data are triples not strings, so this is
    /// open-coded rather than going through the generic helper.
    pub fn resolved_secrets(
        &self,
        own: &[(&str, &str, bool)],
        own_callees: &[&str],
    ) -> Vec<(String, String, bool)> {
        let mut out: Vec<(String, String, bool)> = Vec::new();
        let mut seen: HashSet<(String, String)> = HashSet::new();
        let mut push = |n: &str, k: &str, opt: bool, out: &mut Vec<_>| {
            if seen.insert((n.to_string(), k.to_string())) {
                out.push((n.to_string(), k.to_string(), opt));
            }
        };
        for (n, k, opt) in own {
            push(n, k, *opt, &mut out);
        }
        let mut queue: Vec<&str> = own_callees.to_vec();
        let mut visited: HashSet<&str> = HashSet::new();
        while let Some(c) = queue.pop() {
            if !visited.insert(c) {
                continue;
            }
            if let Some(f) = self.fragments.get(c) {
                for (n, k, opt) in f.secrets {
                    push(n, k, *opt, &mut out);
                }
                queue.extend(f.callees.iter().copied());
            }
        }
        out
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
            ..Default::default()
        }),
        secret_key_secret: Some(api::SecretKeySelector {
            name: s3.secret_key_secret.name.clone(),
            key: s3.secret_key_secret.key.clone(),
            ..Default::default()
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
            from: String::new(),
        })
        .collect()
}

/// `(key, operator, value, effect, toleration_seconds)` — the lowered
/// shape the macro produces for each toleration entry, threaded through
/// `Template::TOLERATIONS_IF_ROOT` and into `WorkflowSpec.Tolerations`
/// by `Collector::stamp_spec`.
pub type TolerationTuple = (String, String, String, String, i64);

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
            from: String::new(),
        })
        .collect()
}

/// Accumulates the reachable templates (as `WorkflowTemplate`s) and the
/// run-mode dispatch table while `Template::collect` walks the closure.
pub struct Collector {
    seen: HashSet<String>,
    /// Deferred so `athena.toml` is read only at emit, never run-mode.
    builders: Vec<fn(&BuildCtx) -> api::Template>,
    runners: HashMap<String, fn(&[String]) -> String>,
    /// `<argo name> -> <on_exit handler argo name>` for *every* template
    /// with an `on_exit` (not just the root). Each WorkflowTemplate
    /// carries its own `spec.hooks.exit`; Argo only fires the hook of
    /// the workflow that is actually submitted (workflow-scoped), so
    /// submitting a sub-workflow's template directly runs its own hook.
    exits: HashMap<String, &'static str>,
    /// `<argo name> -> ttlStrategy` for every template with `ttl(..)`.
    /// Stamped onto that WorkflowTemplate's `spec.ttlStrategy` (same
    /// per-WT, workflow-scoped semantics as `exits`).
    ttl: HashMap<String, crate::api::TtlStrategy>,
    /// `<argo name> -> podGC strategy` for every template with
    /// `pod_gc(..)`. Stamped onto that WT's `spec.podGC`.
    pod_gc: HashMap<String, String>,
    /// `<argo name> -> activeDeadlineSeconds` for every template with
    /// `active_deadline_if_root(..)`. Stamped onto that WT's
    /// `spec.activeDeadlineSeconds` (same per-WT, root-only as `ttl`).
    active_deadline: HashMap<String, i64>,
    /// `<argo name> -> nodeSelector key-value pairs` for every template
    /// that declares `#[workflow(node_selector_if_root = …)]`. Stamped
    /// onto that WT's `spec.nodeSelector` (the only nodeSelector knob
    /// Argo cascades over every pod in the run, root-only).
    node_selector_if_root: HashMap<String, Vec<(String, String)>>,
    /// `<argo name> -> mutexes` for every template declaring
    /// `#[…(mutexes_if_root = […])]`. Each entry is `(name, namespace)`
    /// already lowered to the final YAML form (literal, or
    /// `{{=fromJSON(workflow.parameters[…])}}` for injected operands);
    /// `namespace == ""` means "skip the field" (defaults to the wf's
    /// own ns). Stamped onto that WT's `spec.synchronization.mutexes`.
    mutexes_if_root: HashMap<String, Vec<(String, String)>>,
    /// `<argo name> -> tolerations` for every template declaring
    /// `#[…(tolerations_if_root = [...])]`. Strings already lowered.
    /// Stamped onto that WT's `spec.tolerations`.
    tolerations_if_root: HashMap<String, Vec<TolerationTuple>>,
    /// `<argo name> -> affinity YAML string` for every template with
    /// `#[…(affinity_if_root = "...")]`. Parsed at emit time and
    /// stuffed into `spec.affinity` as a `serde_norway::Value`.
    affinity_if_root: HashMap<String, String>,
    /// `<argo name> -> pod-spec strategic-merge patch (string)` for
    /// every template with `#[workflow(pod_spec_patch_if_root = "...")]`.
    /// Already lowered to its final form (literal, or with
    /// `{{=fromJSON(workflow.parameters[..])}}` injection operands).
    /// Stamped onto that WT's `spec.podSpecPatch`.
    pod_spec_patch_if_root: HashMap<String, String>,
    /// `<argo name> -> Secret names` for every template declaring
    /// `#[…(image_pull_secrets_if_root = [...])]`. Stamped onto that
    /// WT's `spec.imagePullSecrets` as `[{name}]` k8s references.
    image_pull_secrets_if_root: HashMap<String, Vec<String>>,
    /// `<argo name> -> stringified input types` (parallel to the
    /// template's INPUTS), for `container emulate` arg type-checking.
    types: HashMap<String, &'static [&'static str]>,
    /// Argo names of athena-synthesized templates (`Template::SYNTHETIC`)
    /// so `workflow ls` can hide them by default.
    synthetic: HashSet<String>,
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
            exits: HashMap::new(),
            ttl: HashMap::new(),
            pod_gc: HashMap::new(),
            active_deadline: HashMap::new(),
            node_selector_if_root: HashMap::new(),
            mutexes_if_root: HashMap::new(),
            tolerations_if_root: HashMap::new(),
            affinity_if_root: HashMap::new(),
            pod_spec_patch_if_root: HashMap::new(),
            image_pull_secrets_if_root: HashMap::new(),
            types: HashMap::new(),
            synthetic: HashSet::new(),
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

    /// Register a template by type: its `build` fn plus, if it sets
    /// `on_exit_if_root`, its exit handler keyed by Argo name (so
    /// `emit` can put `spec.hooks.exit` on *that* WorkflowTemplate).
    pub fn add<T: Template>(&mut self) {
        self.builders.push(T::build);
        if let Some(handler) = T::ON_EXIT {
            self.exits.insert(T::ARGO_NAME.to_string(), handler);
        }
        if let Some(t) = T::TTL {
            self.ttl.insert(T::ARGO_NAME.to_string(), t);
        }
        if let Some(s) = T::POD_GC {
            self.pod_gc.insert(T::ARGO_NAME.to_string(), s.to_string());
        }
        if let Some(s) = T::ACTIVE_DEADLINE_IF_ROOT {
            self.active_deadline.insert(T::ARGO_NAME.to_string(), s);
        }
        if !T::NODE_SELECTOR_IF_ROOT.is_empty() {
            self.node_selector_if_root.insert(
                T::ARGO_NAME.to_string(),
                T::NODE_SELECTOR_IF_ROOT
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
            );
        }
        if !T::MUTEXES_IF_ROOT.is_empty() {
            self.mutexes_if_root.insert(
                T::ARGO_NAME.to_string(),
                T::MUTEXES_IF_ROOT
                    .iter()
                    .map(|(n, ns)| ((*n).to_string(), (*ns).to_string()))
                    .collect(),
            );
        }
        if !T::TOLERATIONS_IF_ROOT.is_empty() {
            self.tolerations_if_root.insert(
                T::ARGO_NAME.to_string(),
                T::TOLERATIONS_IF_ROOT
                    .iter()
                    .map(|(k, op, v, eff, secs)| {
                        (
                            (*k).to_string(),
                            (*op).to_string(),
                            (*v).to_string(),
                            (*eff).to_string(),
                            *secs,
                        )
                    })
                    .collect(),
            );
        }
        if let Some(a) = T::AFFINITY_IF_ROOT {
            self.affinity_if_root
                .insert(T::ARGO_NAME.to_string(), a.to_string());
        }
        if let Some(p) = T::POD_SPEC_PATCH_IF_ROOT {
            self.pod_spec_patch_if_root
                .insert(T::ARGO_NAME.to_string(), p.to_string());
        }
        if !T::IMAGE_PULL_SECRETS_IF_ROOT.is_empty() {
            self.image_pull_secrets_if_root.insert(
                T::ARGO_NAME.to_string(),
                T::IMAGE_PULL_SECRETS_IF_ROOT
                    .iter()
                    .map(|n| (*n).to_string())
                    .collect(),
            );
        }
        if !T::INPUT_TYPES.is_empty() {
            self.types.insert(T::ARGO_NAME.to_string(), T::INPUT_TYPES);
        }
        if T::SYNTHETIC {
            self.synthetic.insert(T::ARGO_NAME.to_string());
        }
    }

    pub fn add_runner(&mut self, argo_name: &str, run: fn(&[String]) -> String) {
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
    /// The deterministic `WorkflowTemplate` set `emit` serializes —
    /// every reachable template, sorted, with each `on_exit_if_root`
    /// hook stamped on its own template. Shared by YAML emit and the
    /// `CARGO_ATHENA_EMIT_JSON` mode `cargo athena submit` consumes for
    /// its register/drift checks.
    pub fn build_templates(&self, ctx: &BuildCtx) -> Vec<api::WorkflowTemplate> {
        let mut tpls: Vec<api::WorkflowTemplate> = self
            .builders
            .iter()
            .map(|b| {
                let inner = b(ctx);
                wrap_workflow_template(inner.name.clone(), inner)
            })
            .collect();
        tpls.sort_by_key(name_of);

        // Stamp the `*_if_root` family onto each declaring template's
        // own `spec` — `on_exit_if_root` becomes `spec.hooks.exit`
        // (`templateRef`; legacy `spec.onExit` name-string can't cross
        // the one-WT-per-template wormhole), `ttl_if_root`/
        // `pod_gc_if_root`/`active_deadline_if_root`/
        // `node_selector_if_root` land on their matching `spec` fields.
        // Argo fires/applies them workflow-scoped (only for the
        // SUBMITTED root), so per-WT stamping is the correct model: a
        // templateRef'd sub-workflow stays inert when nested but fires
        // on direct submission. Single source of truth lives in
        // `stamp_spec` — the runnable Workflow path in `emit` calls the
        // same method so the two sites can't drift.
        for t in tpls.iter_mut() {
            let name = name_of(t);
            if let Some(spec) = t.spec.as_mut() {
                self.stamp_spec(&name, spec);
            }
        }
        tpls
    }

    /// Apply every per-template spec-scoped attribute (`on_exit_if_root`,
    /// `ttl_if_root`, `pod_gc_if_root`, `active_deadline_if_root`,
    /// `node_selector_if_root`) for `name` onto `spec`. The single
    /// source of truth for the `*_if_root` family — called once per
    /// emitted WT in `build_templates`, and once for the convenience
    /// runnable Workflow's root in `emit`. Adding a new spec-scoped
    /// attribute means adding one map field, one populate line in
    /// `add::<T>()`, and one `if let Some` block here — both
    /// stamping sites pick it up automatically.
    fn stamp_spec(&self, name: &str, spec: &mut api::WorkflowSpec) {
        if let Some(handler) = self.exits.get(name) {
            spec.hooks
                .insert("exit".to_string(), exit_hook_ref(handler));
        }
        if let Some(ttl) = self.ttl.get(name) {
            spec.ttl_strategy = Some(ttl.clone());
        }
        if let Some(s) = self.pod_gc.get(name) {
            spec.pod_gc = Some(api::PodGc {
                strategy: s.clone(),
            });
        }
        if let Some(s) = self.active_deadline.get(name) {
            spec.active_deadline_seconds = Some(*s);
        }
        if let Some(ns) = self.node_selector_if_root.get(name) {
            for (k, v) in ns {
                spec.node_selector.insert(k.clone(), v.clone());
            }
        }
        if let Some(mtx) = self.mutexes_if_root.get(name) {
            let sync = spec
                .synchronization
                .get_or_insert_with(api::Synchronization::default);
            for (mname, mns) in mtx {
                sync.mutexes.push(api::Mutex {
                    name: mname.clone(),
                    namespace: mns.clone(),
                });
            }
        }
        if let Some(tols) = self.tolerations_if_root.get(name) {
            for (k, op, v, eff, secs) in tols {
                spec.tolerations.push(api::Toleration {
                    key: k.clone(),
                    operator: op.clone(),
                    value: v.clone(),
                    effect: eff.clone(),
                    toleration_seconds: if *secs == 0 { None } else { Some(*secs) },
                });
            }
        }
        if let Some(s) = self.affinity_if_root.get(name) {
            spec.affinity = Some(
                serde_norway::from_str(s)
                    .unwrap_or_else(|e| panic!("affinity_if_root: invalid YAML/JSON: {e}")),
            );
        }
        if let Some(p) = self.pod_spec_patch_if_root.get(name) {
            spec.pod_spec_patch = Some(p.clone());
        }
        if let Some(ipss) = self.image_pull_secrets_if_root.get(name) {
            for n in ipss {
                spec.image_pull_secrets
                    .push(api::LocalObjectReference { name: n.clone() });
            }
        }
    }

    pub fn emit<E: Template>(&self, ctx: &BuildCtx, with_workflow: bool) -> String {
        let tpls = self.build_templates(ctx);
        let mut docs: Vec<String> = tpls
            .iter()
            .map(|t| serde_norway::to_string(t).expect("WorkflowTemplate is serializable"))
            .collect();

        if !with_workflow {
            return docs.join("---\n");
        }

        // Start from a default + the two fields specific to the runnable
        // Workflow (`workflowTemplateRef → root`, default SA from
        // athena.toml), then `stamp_spec` overlays every `*_if_root`
        // attribute for the root — same path `build_templates` uses
        // per-WT, so the two stamping sites can never drift when a new
        // `*_if_root` attribute is added.
        let mut spec = api::WorkflowSpec {
            workflow_template_ref: Some(api::WorkflowTemplateRef {
                name: E::ARGO_NAME.to_string(),
                cluster_scope: false,
            }),
            service_account_name: ctx.config().defaults.service_account.clone(),
            ..Default::default()
        };
        self.stamp_spec(E::ARGO_NAME, &mut spec);
        let wf = api::Workflow {
            api_version: api::API_VERSION.to_string(),
            kind: api::KIND_WORKFLOW.to_string(),
            metadata: Some(api::ObjectMeta {
                generate_name: format!("{}-", E::ARGO_NAME),
                ..Default::default()
            }),
            spec: Some(spec),
        };
        docs.push(serde_norway::to_string(&wf).expect("Workflow is serializable"));
        docs.join("---\n")
    }
}

/// A `spec.hooks.exit` `LifecycleHook` referencing the named handler
/// template. The legacy `spec.onExit: <name>` (a name-string) can't
/// cross the one-WT-per-template wormhole on real Argo v4.0.5 — only
/// the structured `templateRef` form survives, hence this single
/// construction reused by every stamping site.
fn exit_hook_ref(handler: &str) -> api::LifecycleHook {
    api::LifecycleHook {
        template_ref: Some(api::TemplateRef {
            name: handler.to_string(),
            template: handler.to_string(),
            cluster_scope: false,
        }),
        ..Default::default()
    }
}

fn name_of(t: &api::WorkflowTemplate) -> String {
    t.metadata
        .as_ref()
        .map(|m| m.name.clone())
        .unwrap_or_default()
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
/// Lowercases, swaps `_` for `-`, and trims leading/trailing `-` so that
/// idiomatic Rust names like `fn _unused_helper()` or `fn foo_()` don't
/// produce DNS-1123-invalid Argo template names (`-foo` / `foo-`, both
/// rejected by k8s). Internal `__` becomes `--` and is kept (valid).
pub fn kebab(s: &str) -> String {
    let s = s.replace('_', "-").to_ascii_lowercase();
    s.trim_matches('-').to_string()
}

/// Hash a `host!` path literal verbatim into a DNS-1123-safe label
/// suffix. Shared between [`volume_name`] and [`host_mount_path`] so
/// the Volume name and the in-container mount path always agree on
/// the same suffix for a given input string.
///
/// **No canonicalization** — `host!("/var/lib")` and `host!("//var/lib")`
/// produce two distinct Volumes (even though Linux resolves them to
/// the same inode). If the user wrote two distinct literals, they get
/// two distinct mounts; letting k8s / Linux handle path resolution
/// keeps our logic simple and removes a category of "did athena
/// silently merge my mounts?" surprises.
///
/// The hash is **FNV-1a 64-bit, fixed initial state**, emitted as 16
/// lowercase hex chars. Determinism is load-bearing: emit-time
/// ([`volume_name`]/[`host_mount_path`] called from `Template::build`)
/// and run-time ([`rt::host_path`]) hash the same literal in two
/// different process invocations (`cargo athena emit` and the in-pod
/// binary); `std::hash::DefaultHasher` uses a per-process random seed
/// and would silently mismatch.
///
/// 16 hex chars = 64 bits. `host-` (5) + 16 = 21-char Volume name,
/// comfortably under DNS-1123's 63-char label limit and collision-
/// resistant well past any plausible per-binary `host!` count.
fn munge_host_path(path: &str) -> String {
    const FNV_OFFSET: u64 = 0xcbf29ce484222325;
    const FNV_PRIME: u64 = 0x100000001b3;
    let mut h = FNV_OFFSET;
    for b in path.bytes() {
        h ^= b as u64;
        h = h.wrapping_mul(FNV_PRIME);
    }
    format!("{h:016x}")
}

fn volume_name(path: &str) -> String {
    format!("host-{}", munge_host_path(path))
}

/// In-container mount path for a `host!("/p")` declaration. Always
/// rooted at [`ATHENA_MOUNTS_DIR`] — `host!` cannot land at the host's
/// own path (see [`ATHENA_MOUNTS_DIR`] for why). Both emit
/// (`host_path_volumes`) and run (`rt::host_path`) go through this
/// helper, so they always agree.
pub fn host_mount_path(host_path: &str) -> String {
    format!("{ATHENA_MOUNTS_DIR}/{}", munge_host_path(host_path))
}

/// `volumes` + `volumeMounts` for a set of hostPaths (from `host!`).
/// Each mounts at [`host_mount_path`] (`/athena/mounts/<munged>`),
/// NOT at the host's own path — safe-by-construction.
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
            mount_path: host_mount_path(p),
            read_only: false,
        });
    }
    (vols, mounts)
}

/// Every container template's volumes/mounts: the always-present
/// `emptyDir` scratch at [`ATHENA_DIR`] + the declared hostPaths. Two
/// hostPath sources:
///
/// - `host_paths` from `host!` — safe-by-construction, mounted at
///   [`host_mount_path`] (`/athena/mounts/<munged>`).
/// - `host_mounts` from `#[container(host_mount = [{…}])]` — explicit
///   `host_path` + `mount_path` + `read_only`, the user's escape hatch
///   for chosen mount paths (`/dev/shm`, sidecar data dirs, …).
///
/// If the same `host_path` appears in both, the `host_mount` entry
/// wins — same Volume, explicit `mount_path`/`read_only`. Keeps the
/// emit free of duplicate Volume names while preserving the user's
/// "I asked for it explicitly" intent.
pub fn container_volumes(
    host_paths: &[String],
    host_mounts: &[(String, String, bool)],
) -> (Vec<api::Volume>, Vec<api::VolumeMount>) {
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
    // host_mount wins over host! on a shared host_path.
    let overridden: HashSet<&str> = host_mounts.iter().map(|(h, _, _)| h.as_str()).collect();
    for p in host_paths {
        if overridden.contains(p.as_str()) {
            continue;
        }
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
            mount_path: host_mount_path(p),
            read_only: false,
        });
    }
    for (host_path, mount_path, read_only) in host_mounts {
        let name = volume_name(host_path);
        vols.push(api::Volume {
            name: name.clone(),
            host_path: Some(api::HostPathVolumeSource {
                path: host_path.clone(),
                r#type: String::new(),
            }),
            ..Default::default()
        });
        mounts.push(api::VolumeMount {
            name,
            mount_path: mount_path.clone(),
            read_only: *read_only,
        });
    }
    (vols, mounts)
}

/// De-reference one argv slot if Argo's emissary rewrote it as a
/// `@/tmp/argo_arg_<i>.txt` sentinel.
///
/// When `c.Args` exceeds 128 KB the controller offloads the whole vector
/// to a `ConfigMap` and clears `c.Args`. The emissary then re-hydrates
/// the args from `$ARGO_CONTAINER_ARGS_FILE` and, separately, replaces
/// any single arg whose length still exceeds 128 KB with a sentinel
/// `@/tmp/argo_arg_<i>.txt` whose contents are the real value
/// (`cmd/argoexec/commands/emissary.go` PR #15265). The container is
/// expected to know that convention and read the file.
///
/// Gating on `$ARGO_CONTAINER_ARGS_FILE` (set by the controller only on
/// offload) keeps this safe: a Regime-B parameter value never starts
/// with `@` (string literals start with `"`, numbers with a digit/sign,
/// bools with `t`/`f`), so even if a user shell wires up a workflow
/// outside Argo we won't mis-interpret a literal `@`-prefixed argv.
fn deref_offloaded_arg(raw: String) -> String {
    if std::env::var_os("ARGO_CONTAINER_ARGS_FILE").is_none() {
        return raw;
    }
    let Some(path) = raw.strip_prefix('@') else {
        return raw;
    };
    if !path.starts_with("/tmp/argo_arg_") {
        return raw;
    }
    std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("failed reading offloaded arg {path}: {e}"))
}

/// The entrypoint a user's `main` calls, parameterised by the root
/// workflow type. Referencing `E` force-links the entire reachable
/// closure (each `collect` calls callees' `collect` directly).
///
/// `krate`/`version`/`bin` identify the *current binary* and are baked
/// into every emitted container's S3 artifact key
/// (`{krate}/{version}/{bin}.tar.gz`). They're captured at the user
/// binary's compile time by the `entrypoint!` macro (the facade
/// crate) from `CARGO_PKG_NAME`/`CARGO_PKG_VERSION`/`CARGO_BIN_NAME`,
/// so `cargo athena publish` (which derives the same key from
/// `cargo metadata` + `--bin`) and the emitted YAML always agree by
/// construction, with no `[artifact]` config field needed.
pub fn entrypoint_impl<E: Template>(krate: &str, version: &str, bin: &str) {
    let mut collector = Collector::new();
    E::collect(&mut collector);

    // Run-mode: `CARGO_ATHENA_TEMPLATE=<name>` selects which template's
    // body to run; the function's parameters arrive as positional argv
    // in INPUTS order (JSON-encoded). The selector lives in env so the
    // pod spec doesn't carry it as a per-template argv string, and so
    // argv is 100% function data, eligible for Argo's automatic offload
    // of large `container.args` to a ConfigMap (env vars are not).
    if let Ok(t) = std::env::var("CARGO_ATHENA_TEMPLATE") {
        let run = *collector
            .runners
            .get(&t)
            .unwrap_or_else(|| panic!("no runnable container template named {t:?}"));
        let argv: Vec<String> = std::env::args().skip(1).map(deref_offloaded_arg).collect();
        let output = run(&argv);
        if let Ok(path) = std::env::var("CARGO_ATHENA_OUTPUT") {
            std::fs::write(path, &output).expect("write CARGO_ATHENA_OUTPUT");
        } else {
            println!("{output}");
        }
        return;
    }

    // `cargo athena container ls` sets this to enumerate every reachable
    // template's metadata as a JSON array (same per-template derivation
    // as describe — so names/params shown are exactly what runs).
    if std::env::var_os("CARGO_ATHENA_LIST").is_some() {
        let ctx = BuildCtx::collect(krate, version, bin);
        let all: Vec<ContainerRunMeta> = collector
            .builders
            .iter()
            .map(|b| {
                let t = b(&ctx);
                let it = collector.types.get(&t.name).copied().unwrap_or(&[]);
                let mut m = ContainerRunMeta::from_template(&t, it);
                m.package = krate.to_string();
                m.synthetic = collector.synthetic.contains(&t.name);
                m
            })
            .collect();
        println!(
            "{}",
            serde_json::to_string(&all).expect("ContainerRunMeta is serializable")
        );
        return;
    }

    // `cargo athena container emulate/describe` sets this to fetch ONE
    // template's metadata as JSON (it then realizes that exact spec
    // locally via docker/podman). Reusing `Template::build` here is what
    // makes the local run identical to Argo by construction — same
    // image, bootstrap, env, volumes, and artifacts as `emit`.
    if let Ok(name) = std::env::var("CARGO_ATHENA_DESCRIBE") {
        let ctx = BuildCtx::collect(krate, version, bin);
        // The default emitted name is `<crate>-<fn>`; the CLI already
        // shows package + short name as separate columns, so accept
        // either the full name or the short form (and fall back to the
        // full name in error messages so the user sees what we tried).
        let full = format!("{krate}-{name}");
        let tpl = collector
            .builders
            .iter()
            .map(|b| b(&ctx))
            .find(|t| t.name == name || t.name == full)
            .unwrap_or_else(|| panic!("no template named {name:?} (or {full:?})"));
        let resolved = tpl.name.clone();
        let input_types = collector.types.get(&resolved).copied().unwrap_or(&[]);
        let mut meta = ContainerRunMeta::from_template(&tpl, input_types);
        meta.package = krate.to_string();
        meta.synthetic = collector.synthetic.contains(&resolved);
        println!(
            "{}",
            serde_json::to_string(&meta).expect("ContainerRunMeta is serializable")
        );
        return;
    }

    // `cargo athena submit` sets this to get the deterministic
    // `WorkflowTemplate` set as a JSON array (structured — for its
    // register-if-missing / drift-detect / apply checks), instead of
    // re-parsing the YAML `emit` prints.
    if std::env::var_os("CARGO_ATHENA_EMIT_JSON").is_some() {
        let ctx = BuildCtx::collect(krate, version, bin);
        println!(
            "{}",
            serde_json::to_string(&collector.build_templates(&ctx))
                .expect("WorkflowTemplate is serializable")
        );
        return;
    }

    // `cargo athena emit --with-workflow` sets this on the child so the
    // convenience runnable Workflow is appended (default: templates
    // only — deterministic, `kubectl apply`-able, GitOps-clean).
    let with_workflow = std::env::var_os("CARGO_ATHENA_WITH_WORKFLOW").is_some_and(|v| v == "1");
    let ctx = BuildCtx::collect(krate, version, bin);
    print!("{}", collector.emit::<E>(&ctx, with_workflow));
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- kebab: Rust ident → DNS-1123-ish Argo name ---------------------

    #[test]
    fn kebab_lowercases_and_hyphenates() {
        assert_eq!(kebab("run_a_container"), "run-a-container");
        assert_eq!(kebab("RunFoo"), "runfoo");
    }

    #[test]
    fn kebab_preserves_digits() {
        // DNS-1123 allows `[a-z0-9]`; digits stay as-is. Rust forbids
        // a leading digit, so we only need to handle internal/trailing.
        assert_eq!(kebab("fetch2"), "fetch2");
        assert_eq!(kebab("step_1_of_3"), "step-1-of-3");
        assert_eq!(kebab("v1_handler"), "v1-handler");
    }

    #[test]
    fn kebab_trims_leading_and_trailing_underscores() {
        // `fn _unused_helper()` is idiomatic Rust (unused-prefix); the
        // kebab MUST trim the leading `-` so make_argo_name doesn't
        // produce `<crate>--unused-helper` (cosmetically ugly, and
        // crate-name-dependent corner cases could land at a literal
        // leading `-`). `fn foo_()` would yield `foo-` (k8s rejects
        // trailing `-`) — trim catches it.
        assert_eq!(kebab("_unused"), "unused");
        assert_eq!(kebab("foo_"), "foo");
        assert_eq!(kebab("_wrapped_"), "wrapped");
        assert_eq!(kebab("__double_"), "double");
    }

    #[test]
    fn kebab_keeps_internal_double_underscore() {
        // Internal `__` lowers to `--`, which is legal DNS-1123. Don't
        // collapse — round-trip back to the source ident is preserved
        // (`foo__bar` ↔ `foo--bar`) so two different Rust idents can't
        // collide in the Argo namespace.
        assert_eq!(kebab("foo__bar"), "foo--bar");
        assert_eq!(kebab("a___b"), "a---b");
    }

    // ---- volume_name / host_mount_path (host! → k8s volume) -------------

    #[test]
    fn munge_is_deterministic_16_hex() {
        // The hash MUST be deterministic across calls (and across
        // process invocations — emit-side and run-side hash the same
        // literal in two different `cargo athena` / in-pod runs and
        // must agree). 16 lowercase hex chars; structurally pinned.
        let m = munge_host_path("/var/lib");
        assert_eq!(m.len(), 16);
        assert!(
            m.chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase())
        );
        assert_eq!(m, munge_host_path("/var/lib"));
    }

    #[test]
    fn munge_known_value_pins_algorithm() {
        // FNV-1a 64-bit with the standard offset/prime, lowercase hex.
        // Pinning a known input → known output so an accidental swap to
        // a different hash function fails LOUD here, not silently in
        // every user's cluster (emit-side and run-side would suddenly
        // mismatch). Bump this intentionally only.
        assert_eq!(munge_host_path("/var/lib"), "5b8d11771a6f946b");
    }

    #[test]
    fn distinct_literals_produce_distinct_volumes() {
        // The whole point of the verbatim-hash design: NO
        // canonicalization. Two strings that Linux resolves identically
        // MUST hash to different Volumes — the user wrote two distinct
        // literals, k8s handles the resolution at mount time.
        assert_ne!(munge_host_path("/var/lib"), munge_host_path("//var/lib"));
        assert_ne!(munge_host_path("/var/lib"), munge_host_path("/var/lib/"));
        assert_ne!(munge_host_path("/var/lib"), munge_host_path("/var//lib"));
    }

    #[test]
    fn volume_name_fits_dns_1123() {
        // k8s Volume names are DNS-1123 LABELS: max 63 chars,
        // `[a-z0-9]([-a-z0-9]*[a-z0-9])?`. `host-` (5) + 16-hex hash =
        // 21 chars total, so any input fits regardless of path length.
        for path in [
            "/",
            "/etc/myapp",
            "/very/deeply/nested/path/that/keeps/going/forever/and/ever/and/ever",
            "/has spaces and weird chars: !@#$%",
        ] {
            let n = volume_name(path);
            assert!(n.len() <= 63, "{n:?} exceeds DNS-1123 label limit");
            assert_eq!(n.len(), 21); // host- + 16 hex
            assert!(n.starts_with("host-"));
            assert!(n.chars().next().unwrap().is_ascii_alphabetic());
            assert!(
                n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-'),
                "{n:?} contains non-DNS-1123 chars"
            );
        }
    }

    #[test]
    fn host_mount_path_agrees_with_volume_name_suffix() {
        // The in-pod mount path and the Volume name MUST agree on the
        // munged suffix (else the VolumeMount wouldn't bind). Both go
        // through munge_host_path; this test pins the contract.
        for path in [
            "/etc/myapp",
            "/var/lib/extra",
            "/srv/123/data",
            "/var/log/app.1",
            "/",
            "//double-slash",
        ] {
            let v = volume_name(path);
            let m = host_mount_path(path);
            let suffix = v.strip_prefix("host-").unwrap();
            assert_eq!(
                m,
                format!("{ATHENA_MOUNTS_DIR}/{suffix}"),
                "path {path:?} produced mismatched volume + mount"
            );
        }
    }

    // ---- secret_env_name (k8s Secret (name, key) → pod env var) ---------

    #[test]
    fn secret_env_name_munges_consistently() {
        // Both halves uppercased; non-alphanumerics → `_`; halves
        // separated by `__` so the two stay distinguishable. The
        // emit-side and run-side both go through this fn — this test
        // pins the contract (drift would silently break `secret!`).
        assert_eq!(
            secret_env_name("my-secret", "db.password"),
            "ATHENA_SEC_MY_SECRET__DB_PASSWORD",
        );
        assert_eq!(secret_env_name("simple", "key"), "ATHENA_SEC_SIMPLE__KEY",);
        // Already-uppercase / digits: pass through.
        assert_eq!(
            secret_env_name("API_v2", "TOKEN_1"),
            "ATHENA_SEC_API_V2__TOKEN_1",
        );
    }

    #[test]
    fn secret_env_name_is_valid_posix_env_var() {
        // POSIX env var names: `[a-zA-Z_][a-zA-Z_0-9]*`. The output of
        // secret_env_name must always satisfy this regardless of what
        // the user passed (any non-alphanumeric → `_`, prefix is
        // `ATHENA_SEC_`, so the first-char rule is always met).
        let valid_env = |s: &str| {
            let mut cs = s.chars();
            cs.next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && cs.all(|c| c.is_ascii_alphanumeric() || c == '_')
        };
        for (name, key) in [
            ("foo", "bar"),
            ("my-secret", "db.password"),
            ("name with spaces", "key/with/slashes"),
            ("-leading-dash", "trailing.dot."),
            ("123-numeric-start", "ok"),
        ] {
            let env = secret_env_name(name, key);
            assert!(valid_env(&env), "{env} is not a valid POSIX env var");
        }
    }
}
