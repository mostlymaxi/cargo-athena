//! Argo Workflows API types — a hand-owned, curated subset.
//!
//! We only emit a narrow, stable slice of Argo (WorkflowTemplate/Workflow;
//! templates with container/dag/steps; artifacts/volumes/params/
//! nodeSelector/SA). These are plain `serde` structs (no protobuf/prost):
//! the IDL bought us nothing here, and conformance is guarded empirically
//! by the kind e2e (`scripts/e2e-test.sh`) running against a real Argo.
//!
//! Serialization rules (so the emitted YAML is Argo-correct):
//! every struct is `rename_all = "camelCase"`, every field is
//! `skip_serializing_if = "ser::skip"` (omit empties) + `default` (for
//! round-trip deserialization).

/// `skip_serializing_if` support: one generic "is this empty?" predicate
/// so every field can share `#[serde(skip_serializing_if = "ser::skip")]`.
pub mod ser {
    use std::collections::{BTreeMap, HashMap};

    /// True when a value is "empty" and should be omitted from output.
    pub trait Skip {
        fn skip(&self) -> bool;
    }

    impl Skip for String {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl Skip for bool {
        fn skip(&self) -> bool {
            !*self
        }
    }
    impl Skip for i32 {
        fn skip(&self) -> bool {
            *self == 0
        }
    }
    impl<T> Skip for Option<T> {
        fn skip(&self) -> bool {
            self.is_none()
        }
    }
    impl<T> Skip for Vec<T> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl<K, V> Skip for HashMap<K, V> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }
    impl<K, V> Skip for BTreeMap<K, V> {
        fn skip(&self) -> bool {
            self.is_empty()
        }
    }

    /// The function named in every field's `skip_serializing_if`.
    pub fn skip<T: Skip>(value: &T) -> bool {
        value.skip()
    }
}

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// `#[derive]` + `serde` boilerplate shared by every message, and a
/// `skip`/`default` field attribute on each field.
macro_rules! argo {
    ($(
        $(#[$m:meta])*
        pub struct $name:ident { $(
            $(#[$fm:meta])*
            pub $fld:ident : $ty:ty
        ),* $(,)? }
    )*) => {$(
        $(#[$m])*
        #[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
        #[serde(rename_all = "camelCase")]
        pub struct $name {
            $(
                $(#[$fm])*
                #[serde(default, skip_serializing_if = "crate::ser::skip")]
                pub $fld : $ty,
            )*
        }
    )*};
}

argo! {
    pub struct Workflow {
        pub api_version: String,
        pub kind: String,
        pub metadata: Option<ObjectMeta>,
        pub spec: Option<WorkflowSpec>,
    }

    /// A reusable, independently-addressable template resource. Every
    /// `#[workflow]`/`#[container]` emits one; cross-template calls
    /// reference it by name via `TemplateRef`.
    pub struct WorkflowTemplate {
        pub api_version: String,
        pub kind: String,
        pub metadata: Option<ObjectMeta>,
        pub spec: Option<WorkflowSpec>,
    }

    pub struct ObjectMeta {
        pub name: String,
        pub generate_name: String,
        pub namespace: String,
        pub labels: BTreeMap<String, String>,
        pub annotations: BTreeMap<String, String>,
    }

    pub struct WorkflowSpec {
        pub entrypoint: String,
        pub templates: Vec<Template>,
        pub arguments: Option<Arguments>,
        /// Set on a runnable Workflow that just invokes a WorkflowTemplate.
        pub workflow_template_ref: Option<WorkflowTemplateRef>,
        pub service_account_name: String,
        /// Root-scoped pod scheduling for the *submitted* Workflow
        /// (Argo applies it to every pod). Only `cargo athena submit
        /// --node-selector` sets this; emit never does (skip-empty ⇒
        /// existing goldens unaffected).
        pub node_selector: BTreeMap<String, String>,
        /// Whole-workflow lifecycle hooks. Key `exit` is the exit handler
        /// (runs once when the Workflow finishes). We use this (with a
        /// `templateRef`) rather than the legacy `spec.onExit` string,
        /// which only resolves a *local* template name — unusable across
        /// the one-WT-per-template wormhole.
        pub hooks: BTreeMap<String, LifecycleHook>,
        /// Workflow-scoped TTL GC (`#[…(ttl(..))]`).
        pub ttl_strategy: Option<TtlStrategy>,
        /// Workflow-scoped pod GC (`#[…(pod_gc(strategy=..))]`). camelCase
        /// of `pod_gc` is `podGc`, but Argo's field is `podGC` — the
        /// `argo!` macro forwards this explicit rename ahead of its
        /// `rename_all`, so it wins.
        #[serde(rename = "podGC")]
        pub pod_gc: Option<PodGc>,
        /// Root-scoped Argo `WorkflowSpec.activeDeadlineSeconds` — the
        /// genuine whole-workflow runtime cap, from
        /// `#[…(active_deadline_if_root=..)]`. (`int64` in Argo;
        /// camelCase of the field name already matches.) skip-empty ⇒
        /// existing goldens stay byte-identical.
        pub active_deadline_seconds: Option<i64>,
    }

    /// Argo `ttlStrategy`: delete the finished Workflow after the given
    /// seconds. Each bound is independent (`#[…(ttl(after_completion=…,
    /// after_success=…, after_failure=…))]`).
    pub struct TtlStrategy {
        pub seconds_after_completion: Option<i32>,
        pub seconds_after_success: Option<i32>,
        pub seconds_after_failure: Option<i32>,
    }

    /// Argo `podGC`: when to delete the Workflow's pods.
    pub struct PodGc {
        pub strategy: String,
    }

    /// Points a runnable Workflow at a WorkflowTemplate resource.
    pub struct WorkflowTemplateRef {
        pub name: String,
        pub cluster_scope: bool,
    }

    /// A DAG task's reference to a template in another WorkflowTemplate.
    pub struct TemplateRef {
        pub name: String,
        pub template: String,
        pub cluster_scope: bool,
    }

    pub struct Template {
        pub name: String,
        /// Argo `Template.metadata` — annotations + labels on the
        /// pod/dag/steps template. Optional, skip-serialized when
        /// empty, so existing goldens stay byte-identical for
        /// templates that don't use the `annotations = {…}` attr.
        pub metadata: Option<ObjectMeta>,
        pub inputs: Option<Inputs>,
        pub outputs: Option<Outputs>,
        // Exactly one of the following describes the template body.
        pub container: Option<Container>,
        pub dag: Option<DagTemplate>,
        pub script: Option<ScriptTemplate>,
        pub volumes: Vec<Volume>,
        /// Per-template SA override (Argo runs the pod as this).
        pub service_account_name: String,
        /// Template-level pod scheduling (container templates).
        pub node_selector: BTreeMap<String, String>,
        /// `#[workflow(steps)]` body: Argo `steps` is a list of lists —
        /// inner runs in parallel, outer sequentially. Plain serde nests
        /// `Vec<Vec<_>>` natively (no proto wrapper needed).
        pub steps: Vec<Vec<DagTask>>,
        /// Template-level retry policy (`#[container/workflow(retry(..))]`).
        pub retry_strategy: Option<RetryStrategy>,
        /// Template-level timeout duration (`#[…(timeout = "5m")]`).
        pub timeout: String,
        /// Template-level deadline (`#[…(active_deadline = …)]`) →
        /// Argo `Template.activeDeadlineSeconds` (per-pod; applies even
        /// when this template is `templateRef`'d — NOT root-only).
        pub active_deadline_seconds: Option<i32>,
    }

    /// Argo `retryStrategy`: re-run the template on failure. Nil `limit`
    /// == unlimited; `retry_policy` empty == Argo default (`OnFailure`).
    pub struct RetryStrategy {
        pub limit: Option<i32>,
        pub retry_policy: String,
        pub backoff: Option<Backoff>,
    }

    /// Exponential back-off between retries.
    pub struct Backoff {
        pub duration: String,
        pub factor: Option<i32>,
        pub max_duration: String,
    }

    pub struct Inputs {
        pub parameters: Vec<Parameter>,
        pub artifacts: Vec<Artifact>,
    }

    pub struct Outputs {
        pub parameters: Vec<Parameter>,
        pub artifacts: Vec<Artifact>,
    }

    pub struct Parameter {
        pub name: String,
        pub value: Option<String>,
        pub default: Option<String>,
        pub value_from: Option<ValueFrom>,
    }

    pub struct ValueFrom {
        pub path: String,
        pub parameter: String,
        /// An Argo expr (`expr-lang`) evaluated after the DAG/steps
        /// finish. Used by a synthesized `if` wrapper to select the
        /// taken branch's `return` (skip-serialized so unaffected
        /// templates stay byte-identical).
        pub expression: String,
    }

    pub struct Artifact {
        pub name: String,
        pub path: String,
        /// Where the artifact lives (binary tarball / load-save ports).
        pub s3: Option<S3Artifact>,
        /// `none` => deliver the raw object; bootstrap untars itself.
        pub archive: Option<ArchiveStrategy>,
        /// Octal file mode applied to the downloaded file.
        pub mode: Option<i32>,
    }

    /// Mirrors a k8s SecretKeySelector — a key in a Secret. `optional`
    /// is K8s's "don't fail pod-start if missing" flag, surfaced by
    /// `cargo_athena::secret_opt!` (skip-serialized when false, so
    /// existing S3Artifact users stay byte-identical).
    pub struct SecretKeySelector {
        pub name: String,
        pub key: String,
        pub optional: bool,
    }

    /// Mirrors Argo's S3Artifact.
    pub struct S3Artifact {
        pub endpoint: String,
        pub bucket: String,
        pub region: String,
        pub insecure: bool,
        pub key: String,
        pub access_key_secret: Option<SecretKeySelector>,
        pub secret_key_secret: Option<SecretKeySelector>,
    }

    pub struct ArchiveStrategy {
        /// Present (and empty) means "do not archive/extract".
        pub none: Option<NoneStrategy>,
    }

    pub struct NoneStrategy {}

    pub struct Arguments {
        pub parameters: Vec<Parameter>,
        pub artifacts: Vec<Artifact>,
    }

    pub struct DagTemplate {
        pub tasks: Vec<DagTask>,
    }

    pub struct DagTask {
        pub name: String,
        /// Empty when `template_ref` is set.
        pub template: String,
        pub dependencies: Vec<String>,
        pub arguments: Option<Arguments>,
        pub template_ref: Option<TemplateRef>,
        // Declared last + skip-if-empty so tasks that use neither leave
        // every existing golden byte-identical.
        pub continue_on: Option<ContinueOn>,
        /// Argo lifecycle hooks: arbitrary key -> hook. Key `exit` is the
        /// special unconditional on-completion hook; others fire when
        /// their `expression` holds.
        pub hooks: BTreeMap<String, LifecycleHook>,
        /// Fan-out: a JSON-array string; the task runs once per element
        /// with `{{item}}` bound. Empty == no fan-out (skip-serialized).
        pub with_param: String,
        /// Conditional execution: an Argo expr (`expr-lang`). The task
        /// runs only when it evaluates truthy; else it is Skipped. Empty
        /// == unconditional (skip-serialized so existing goldens are
        /// byte-identical).
        pub when: String,
    }

    /// Proceed to dependents even if this task fails/errors.
    pub struct ContinueOn {
        pub error: bool,
        pub failed: bool,
    }

    /// A hook that runs a template on a lifecycle event. `expression`
    /// empty == the special `exit` hook (runs on completion).
    pub struct LifecycleHook {
        pub template_ref: Option<TemplateRef>,
        pub arguments: Option<Arguments>,
        pub expression: String,
    }

    pub struct Container {
        pub image: String,
        pub command: Vec<String>,
        pub args: Vec<String>,
        pub env: Vec<EnvVar>,
        pub volume_mounts: Vec<VolumeMount>,
        pub working_dir: String,
        /// K8s `securityContext` on this container. Only `privileged`
        /// is exposed today (`#[container(privileged = true)]`); other
        /// fields can join when there's a real use case. Skip-empty
        /// keeps existing goldens byte-identical.
        pub security_context: Option<SecurityContext>,
    }

    /// K8s `SecurityContext` on a container. Minimal: only the fields
    /// we expose. Each field skip-serializes its default so the
    /// produced YAML stays terse.
    pub struct SecurityContext {
        pub privileged: bool,
    }

    pub struct ScriptTemplate {
        pub image: String,
        pub command: Vec<String>,
        pub source: String,
    }

    pub struct EnvVar {
        pub name: String,
        pub value: String,
        /// Pulled from a `valueFrom` source instead of a literal. Used
        /// by `cargo_athena::secret!`/`secret_opt!` (secretKeyRef).
        pub value_from: Option<EnvVarSource>,
    }

    /// Argo `EnvVarSource`. Only `secretKeyRef` is exposed today; this
    /// can grow as we surface more (configMapKeyRef, fieldRef, …).
    pub struct EnvVarSource {
        pub secret_key_ref: Option<SecretKeySelector>,
    }

    pub struct Volume {
        pub name: String,
        pub host_path: Option<HostPathVolumeSource>,
        pub empty_dir: Option<EmptyDirVolumeSource>,
    }

    pub struct HostPathVolumeSource {
        pub path: String,
        pub r#type: String,
    }

    /// Present (and empty) => a pod-scoped scratch dir (`emptyDir: {}`).
    pub struct EmptyDirVolumeSource {}

    pub struct VolumeMount {
        pub name: String,
        pub mount_path: String,
        pub read_only: bool,
    }
}

/// Argo's `apiVersion` for `Workflow`/`WorkflowTemplate` resources.
pub const API_VERSION: &str = "argoproj.io/v1alpha1";
/// Argo's `kind` for `Workflow` resources.
pub const KIND_WORKFLOW: &str = "Workflow";
/// Argo's `kind` for `WorkflowTemplate` resources.
pub const KIND_WORKFLOW_TEMPLATE: &str = "WorkflowTemplate";
