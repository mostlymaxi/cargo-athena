//! Attribute parsers + their token lowerings.
//!
//! - [`ContainerArgs`] / [`WorkflowArgs`] — top-level deluxe-parsed
//!   attribute structs.
//! - [`RetryArgs`] / [`TtlArgs`] / [`PodGcArgs`] / [`HostMountEntry`] —
//!   nested groups (the first three are hand-implemented `ParseMetaItem`
//!   because deluxe's derive only accepts the brace form, not `name(…)`).
//! - [`parse_attr`] — entry point used by every attribute macro.
//! - Token-lowering fns:
//!   [`retry_strategy_tokens`] / [`ttl_const_tokens`] /
//!   [`pod_gc_const_tokens`] / [`timeout_tokens`] / [`secs_i32_tok`] /
//!   [`secs_i64_tok`].
//! - [`parse_duration_secs`] — the single duration parser shared by every
//!   duration-bearing attribute (int seconds or humantime string).
//! - [`inject_lower`] — lowers `"lit" + arg + arg.field` attribute values
//!   to Argo `{{=fromJSON(...)}}` template strings; shared by container
//!   (image/sa/node_selector/env/annotations) and workflow
//!   (node_selector_if_root) attrs.

use proc_macro::TokenStream;
use quote::quote;
use syn::Expr;

use crate::utils::unwrap_expr;

/// `retry(limit = N | unlimited, policy = "...", backoff = "<dur>")` —
/// template-level Argo `retryStrategy`. `limit` is required (enforced in
/// `retry_strategy_tokens`, not the parse); `policy`/`backoff` optional.
///
/// Manual `ParseMetaItem` (not `#[derive]`): the spec mandates the
/// **paren** call form `retry(limit = …, …)`. deluxe's derived
/// `ParseMetaItem` only accepts the *brace* form for a struct field
/// (`retry { … }`); for `name(…)` it routes through
/// `parse_meta_item_inline`, whose derived body still demands curly
/// braces ("expected curly braces"). Hand-parsing the comma-separated
/// `ident = value` pairs from the parenthesized buffer (public `syn`
/// only — no deluxe internals) gives exactly the spec'd grammar.
#[derive(Default)]
pub(crate) struct RetryArgs {
    limit: Option<syn::Expr>,
    /// Kept as `LitStr` (not `String`) so the unknown-policy error can
    /// span the offending literal instead of the fn ident.
    policy: Option<syn::LitStr>,
    backoff: Option<syn::Expr>,
}

impl RetryArgs {
    /// Parse the comma-separated `ident = value` pairs from a buffer
    /// (the parenthesized `retry( … )` content).
    fn parse_fields(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut out = RetryArgs::default();
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            match key.to_string().as_str() {
                "limit" => out.limit = Some(input.parse::<syn::Expr>()?),
                "policy" => {
                    out.policy = Some(input.parse::<syn::LitStr>()?);
                }
                "backoff" => out.backoff = Some(input.parse::<syn::Expr>()?),
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "unknown `retry(...)` field `{other}` \
                             (expected limit, policy, backoff)"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(out)
    }
}

impl deluxe::ParseMetaItem for RetryArgs {
    fn parse_meta_item(
        input: syn::parse::ParseStream,
        _mode: deluxe::ParseMode,
    ) -> deluxe::Result<Self> {
        // Reached for the `retry( … )` paren form: deluxe (via
        // `Option<T>` → `Paren::parse_delimited_meta_item`) hands us
        // the already-unwrapped parenthesized buffer, so the bare
        // comma-separated `ident = value` field list is parsed here.
        // Also accept an explicit `{ … }` brace group for symmetry.
        if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            return RetryArgs::parse_fields(&content);
        }
        RetryArgs::parse_fields(input)
    }

    fn missing_meta_item(_name: &str, _span: proc_macro2::Span) -> deluxe::Result<Self> {
        Ok(RetryArgs::default())
    }
}

/// `ttl_if_root(after_completion = <secs>, after_success = <secs>,
/// after_failure = <secs>)` — root-only WorkflowSpec Argo `ttlStrategy`.
/// All three optional, ≥1 required (enforced in `ttl_const_tokens`).
/// Hand-parsed for the same reason as `RetryArgs` (deluxe's derived
/// `ParseMetaItem` only does the brace form, not `name(…)`).
#[derive(Default)]
pub(crate) struct TtlArgs {
    after_completion: Option<syn::Expr>,
    after_success: Option<syn::Expr>,
    after_failure: Option<syn::Expr>,
}

impl TtlArgs {
    fn parse_fields(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut out = TtlArgs::default();
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            match key.to_string().as_str() {
                "after_completion" => out.after_completion = Some(input.parse::<syn::Expr>()?),
                "after_success" => out.after_success = Some(input.parse::<syn::Expr>()?),
                "after_failure" => out.after_failure = Some(input.parse::<syn::Expr>()?),
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "unknown `ttl_if_root(...)` field `{other}` (expected \
                             after_completion, after_success, after_failure)"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(out)
    }
}

impl deluxe::ParseMetaItem for TtlArgs {
    fn parse_meta_item(
        input: syn::parse::ParseStream,
        _mode: deluxe::ParseMode,
    ) -> deluxe::Result<Self> {
        if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            return TtlArgs::parse_fields(&content);
        }
        TtlArgs::parse_fields(input)
    }

    fn missing_meta_item(_name: &str, _span: proc_macro2::Span) -> deluxe::Result<Self> {
        Ok(TtlArgs::default())
    }
}

/// `pod_gc_if_root(strategy = "<S>")` — root-only WorkflowSpec `podGC`.
/// `strategy` is required + must be a known strategy (enforced in
/// `pod_gc_const_tokens`). Hand-parsed for the same reason as
/// `RetryArgs`.
#[derive(Default)]
pub(crate) struct PodGcArgs {
    /// `LitStr` so the unknown-strategy error spans the literal.
    strategy: Option<syn::LitStr>,
}

impl PodGcArgs {
    fn parse_fields(input: syn::parse::ParseStream) -> syn::Result<Self> {
        let mut out = PodGcArgs::default();
        while !input.is_empty() {
            let key: syn::Ident = input.parse()?;
            input.parse::<syn::Token![=]>()?;
            match key.to_string().as_str() {
                "strategy" => {
                    out.strategy = Some(input.parse::<syn::LitStr>()?);
                }
                other => {
                    return Err(syn::Error::new_spanned(
                        &key,
                        format!(
                            "unknown `pod_gc_if_root(...)` field `{other}` (expected strategy)"
                        ),
                    ));
                }
            }
            if !input.is_empty() {
                input.parse::<syn::Token![,]>()?;
            }
        }
        Ok(out)
    }
}

impl deluxe::ParseMetaItem for PodGcArgs {
    fn parse_meta_item(
        input: syn::parse::ParseStream,
        _mode: deluxe::ParseMode,
    ) -> deluxe::Result<Self> {
        if input.peek(syn::token::Brace) {
            let content;
            syn::braced!(content in input);
            return PodGcArgs::parse_fields(&content);
        }
        PodGcArgs::parse_fields(input)
    }

    fn missing_meta_item(_name: &str, _span: proc_macro2::Span) -> deluxe::Result<Self> {
        Ok(PodGcArgs::default())
    }
}

/// `#[container(image = "...", name = "...", service_account = "...",
///   node_selector = { "k" = "v", ... })]`
///
/// `image`/`service_account` and `node_selector` *values* are
/// expressions: a string literal, or a `+`-concatenation of string
/// literals and container args / their named fields (param injection —
/// see `inject_lower`). `name` is the static Argo template name;
/// `node_selector` *keys* are literal (the `String` type enforces it).
/// `host_mount = [{ host_path = "/h", mount_path = "/m", read_only = false }]`
/// — explicit, chosen-path hostPath mount. The pair to `host!`, which
/// always lands at `/athena/mounts/<munged>` for safety. Use this
/// when you really do want a specific in-container mount path
/// (`/dev/shm`, a sidecar's data dir, etc.).
#[derive(deluxe::ParseMetaItem)]
pub(crate) struct HostMountEntry {
    /// `LitStr` so the absolute-path checks span the literal.
    pub(crate) host_path: syn::LitStr,
    pub(crate) mount_path: syn::LitStr,
    #[deluxe(default)]
    pub(crate) read_only: bool,
}

/// `mutexes = [{ name = "x", namespace = "ns" }, …]` /
/// `mutexes_if_root = […]` — one entry of either list. Both `name` and
/// `namespace` accept the same `"lit" + arg + arg.field` injection
/// grammar as the other injectable string attrs; lowering scope differs
/// by attr (`inputs.parameters` for template-level `mutexes`,
/// `workflow.parameters` for `mutexes_if_root` — see the call sites).
/// Empty namespace ⇒ Argo defaults to the workflow's own ns
/// (`workflow/sync/lock_name.go:58-67`).
#[derive(deluxe::ParseMetaItem)]
pub(crate) struct MutexArg {
    pub(crate) name: syn::Expr,
    #[deluxe(default)]
    pub(crate) namespace: Option<syn::Expr>,
}

/// `tolerations = [{ key, operator, value, effect, toleration_seconds }, …]`
/// — K8s `Toleration` entry. `key`, `value`, `effect` accept the same
/// `"lit" + arg` injection grammar as `image`/`env` (scope per attr:
/// `inputs.parameters` for container-level, `workflow.parameters` for
/// `_if_root`). `operator` is a literal string (closed set `"Equal"` |
/// `"Exists"`, checked at compile time); a literal `effect` is checked
/// against K8s's closed set too (an injected/substituted effect is
/// checked by k8s at admission instead). `toleration_seconds` is a
/// literal i64 (0 ⇒ skip-serialize, k8s default "applies forever").
#[derive(deluxe::ParseMetaItem)]
pub(crate) struct TolerationArg {
    pub(crate) key: syn::Expr,
    pub(crate) operator: syn::LitStr,
    #[deluxe(default)]
    pub(crate) value: Option<syn::Expr>,
    pub(crate) effect: syn::Expr,
    #[deluxe(default)]
    pub(crate) toleration_seconds: i64,
}

/// Literal-only `TolerationArg`: same shape, but every field is a
/// plain literal (no `syn::Expr`, no injection). Used for
/// `boundary_tolerations` (Argo's boundary tier reads tolerations
/// before per-template substitution gets a chance, so injection is
/// unsafe by construction — same rationale as `boundary_node_selector`).
#[derive(deluxe::ParseMetaItem)]
pub(crate) struct BoundaryTolerationArg {
    pub(crate) key: String,
    /// `LitStr` so the closed-set checks span the offending literal.
    pub(crate) operator: syn::LitStr,
    #[deluxe(default)]
    pub(crate) value: String,
    pub(crate) effect: syn::LitStr,
    #[deluxe(default)]
    pub(crate) toleration_seconds: i64,
}

#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
pub(crate) struct ContainerArgs {
    pub(crate) image: Option<syn::Expr>,
    /// `LitStr` so the DNS-1123 / YAML-safety checks span the literal.
    pub(crate) name: Option<syn::LitStr>,
    pub(crate) service_account: Option<syn::Expr>,
    pub(crate) node_selector: std::collections::BTreeMap<String, syn::Expr>,
    /// `on_exit_if_root = path::to::template` — whole-workflow exit
    /// handler on this template's own `spec.hooks.exit`; Argo fires it
    /// only when this workflow is the one submitted (the run's root).
    /// Named distinctly from the per-task `.on_exit(t)` builder.
    pub(crate) on_exit_if_root: Option<syn::Path>,
    /// Template-level `retryStrategy` (`limit` required when present).
    pub(crate) retry: Option<RetryArgs>,
    /// `timeout = <secs | "1h30m">` → Argo `Template.timeout`
    /// (controller-enforced node timeout, counts Pending time). Int =
    /// seconds, or a humantime string. `#[container]`-only — Argo
    /// documents it as a no-op on dag/steps templates.
    pub(crate) timeout: Option<syn::Expr>,
    /// `pod_running_timeout = <secs | "1h30m">` → the pod's
    /// `Template.activeDeadlineSeconds` (kubelet hard-kills the pod
    /// once Running). `#[container]`-only (Argo: container/script only).
    pub(crate) pod_running_timeout: Option<syn::Expr>,
    /// Root-only WorkflowSpec TTL GC (`ttl_if_root(after_completion=…)`).
    pub(crate) ttl_if_root: Option<TtlArgs>,
    /// Root-only WorkflowSpec pod GC (`pod_gc_if_root(strategy=…)`).
    pub(crate) pod_gc_if_root: Option<PodGcArgs>,
    /// Root-only whole-workflow runtime cap →
    /// `WorkflowSpec.activeDeadlineSeconds` (`active_deadline_if_root =
    /// <secs | "2h">`). The only real workflow timeout.
    pub(crate) active_deadline_if_root: Option<syn::Expr>,
    /// `env = { "KEY" = "lit" + arg, … }` — extra container `env` entries
    /// the body can read via `std::env::var(…)`. Literal keys; values
    /// follow the same `"lit" + arg + …` injection grammar as
    /// `image`/`service_account`/`node_selector`.
    pub(crate) env: std::collections::BTreeMap<String, syn::Expr>,
    /// `host_mount = [{ host_path = "/h", mount_path = "/m", read_only =
    /// false }]` — explicit, chosen-path hostPath mounts. Use when you
    /// genuinely need a specific in-container path (`host!`'s safe
    /// `/athena/mounts/<munged>` is the default). Volumes are deduped
    /// against `host!`-declared paths so combining the two never
    /// double-mounts.
    pub(crate) host_mount: Vec<HostMountEntry>,
    /// `annotations = { "key" = "lit" + arg, … }` — pod annotations.
    /// Literal keys, injectable values (same grammar as `env` /
    /// `node_selector`).
    pub(crate) annotations: std::collections::BTreeMap<String, syn::Expr>,
    /// `privileged = true` — K8s `securityContext.privileged: true` on
    /// this container. Off by default; opt in only when you genuinely
    /// need host devices / kernel-level access (mounting NVIDIA gear,
    /// running `iptables`, …). The cluster's PodSecurityPolicy /
    /// PodSecurity admission still has the final say.
    pub(crate) privileged: bool,
    /// `daemon` / `daemon = true` — Argo `Template.daemon: true`. The pod
    /// runs long-lived: the workflow proceeds to dependent tasks once the
    /// container reaches readiness (not completion), and Argo terminates it
    /// when the enclosing dag/steps finishes. Container-only; `#[workflow]`
    /// has no `daemon` field so `#[workflow(daemon)]` is a deluxe
    /// unknown-field error by construction. A daemon that exits `Succeeded`
    /// is marked FAILED, and `retry` only covers startup — see CONTAINER.md.
    pub(crate) daemon: bool,
    /// `mutexes = [{ name = "x", namespace = "ns" }, …]` — template-level
    /// `Template.synchronization.mutexes`. Holder key
    /// `<ns>/<wf>/<node>`: serialize nodes referencing this template
    /// within ONE run AND across separate Workflow runs sharing the
    /// same mutex name + namespace (Argo's sync manager is global per
    /// namespace per controller). `name`/`namespace` accept the same
    /// `"lit" + arg + arg.field` injection as `image`/`env` — scope
    /// `inputs.parameters` (per-step substitution, empirically safe at
    /// `Template.synchronization` on v4.0.5, no nodeSelector-style
    /// boundary-copy footgun).
    pub(crate) mutexes: Vec<MutexArg>,
    /// `mutexes_if_root = [{ name = "x", namespace = "ns" }, …]` —
    /// root-only `WorkflowSpec.synchronization.mutexes`. Holder key
    /// `<ns>/<wf>`: serializes whole separate Workflow runs against
    /// each other. Same per-WT, root-only plumbing as
    /// `ttl_if_root`/`pod_gc_if_root`/`active_deadline_if_root`; inert
    /// when this WT is `templateRef`'d as a sub-workflow. Injection
    /// scope `workflow.parameters` (the only form Argo resolves at
    /// `WorkflowSpec` scope).
    pub(crate) mutexes_if_root: Vec<MutexArg>,
    /// `tolerations = [{ key, operator, value, effect, ... }, …]` →
    /// `Template.Tolerations` on this container's WT. Strings accept
    /// the `"lit" + arg` injection grammar lowered against
    /// `inputs.parameters` — safe at template scope (the leaf pod
    /// renders from this template's own substituted form, no
    /// nodeSelector-style boundary-copy footgun; empirically verified
    /// v4.0.5 2026-05-26).
    pub(crate) tolerations: Vec<TolerationArg>,
    /// `tolerations_if_root = [...]` → root-only `WorkflowSpec
    /// .Tolerations` (3rd tier of Argo's `tmpl → boundary → wfSpec`
    /// pod-scheduling lookup). Injection scope `workflow.parameters`.
    /// Same `_if_root` family as `mutexes_if_root`.
    pub(crate) tolerations_if_root: Vec<TolerationArg>,
    /// `affinity = "<json|yaml>"` → `Template.Affinity` on this
    /// container's WT, as an opaque YAML/JSON string. Athena does NOT
    /// model `apiv1.Affinity`'s deeply-nested schema by design (use
    /// `pod_spec_patch` if a typed approach matters). Substitution at
    /// this scope is safe for the leaf pod (same path as `pod_spec
    /// _patch`).
    pub(crate) affinity: Option<syn::Expr>,
    /// `affinity_if_root = "<json|yaml>"` → root-only `WorkflowSpec
    /// .Affinity`. Same `_if_root` family. Users can hand-write
    /// `{{workflow.parameters.X}}` substitutions inside the string.
    pub(crate) affinity_if_root: Option<syn::Expr>,
    /// `pod_spec_patch = "<json|yaml>"` — `Template.PodSpecPatch` on
    /// this container's WT. A strategic-merge patch applied to the
    /// rendered pod just before submission; the universal escape
    /// hatch for any podSpec field athena hasn't lifted to a
    /// first-class attr (resources, sidecars, init containers,
    /// fsGroup, etc.). String accepts the `"lit" + arg + arg.field`
    /// injection grammar; operands lower to
    /// `{{=fromJSON(inputs.parameters[..])}}` and resolve at pod-
    /// creation (`processPodSpecPatch` → `ProcessArgs`, no
    /// nodeSelector-style boundary-copy footgun — proven v4.0.5
    /// 2026-05-26).
    pub(crate) pod_spec_patch: Option<syn::Expr>,
    /// `pod_spec_patch_if_root = "<json|yaml>"` — root-only
    /// `WorkflowSpec.PodSpecPatch`, fires only when this container is
    /// the submitted root (same `_if_root` family as `mutexes_if_root`).
    /// Argo concats this with the per-template `pod_spec_patch` and
    /// applies the merge to every pod in the run. Injection scope
    /// `workflow.parameters` — the only form Argo resolves at
    /// WorkflowSpec.
    pub(crate) pod_spec_patch_if_root: Option<syn::Expr>,
    /// `image_pull_secrets_if_root = ["regcred", "harborcred"]` — root-
    /// only `WorkflowSpec.ImagePullSecrets`, the Secret names the
    /// kubelet uses to pull every pod's image from a private registry.
    /// K8s / Argo expose this only at workflow scope; per-container
    /// needs go through `pod_spec_patch`. Literal names only.
    pub(crate) image_pull_secrets_if_root: Vec<String>,
}

/// `#[workflow(name = "...", steps,
///   boundary_node_selector = { "k" = "v", ... },
///   node_selector_if_root = { "k" = "v", "k2" = "lit" + arg, ... },
///   on_exit_if_root = teardown)]` — bare `steps` opts into Argo
/// `steps:` (sequential) vs the default `dag:`.
///
/// **Two nodeSelector knobs, different tiers** (Argo's pod-creation
/// fallback at `workflow/controller/workflowpod.go:928-958`):
///
/// * `boundary_node_selector` → `Template.NodeSelector` on this
///   dag/steps. Used only as the *immediate* boundary fallback — does
///   NOT cascade through nested sub-workflows. **Literal-only** by
///   design: per-arg injection here would have to lower to
///   `workflow.parameters` (root-scoped) and then a `templateRef`'d
///   sub's value would surprise-resolve against the SUBMITTED ROOT's
///   args, not this template's inputs. Keep boundary selectors static;
///   use `node_selector_if_root` (below) for dynamic values.
///
/// * `node_selector_if_root` → `WorkflowSpec.NodeSelector`. Applies to
///   every pod in the run by default. **Root-only** (inert when this
///   WT is `templateRef`'d — same family as `ttl_if_root` /
///   `pod_gc_if_root` / `active_deadline_if_root`). Supports the same
///   `"lit" + arg` / `"lit" + arg.field` injection grammar as
///   `#[container]`, but lowers to
///   `{{=fromJSON(workflow.parameters['arg'])}}` (root-scoped — the
///   ONLY form Argo resolves at WorkflowSpec scope on v4.0.5).
///   `inputs.parameters` is empirically inert at WorkflowSpec scope
///   and at Template.NodeSelector via boundary fallback (the raw
///   string lands on the child pod before any per-template
///   substitution can resolve it — proven 2026-05-24).
#[derive(deluxe::ParseMetaItem, Default)]
#[deluxe(default)]
pub(crate) struct WorkflowArgs {
    /// `LitStr` so the DNS-1123 / YAML-safety checks span the literal.
    pub(crate) name: Option<syn::LitStr>,
    pub(crate) steps: deluxe::Flag,
    /// `boundary_node_selector = { "k" = "v" }` — Argo
    /// `Template.NodeSelector` on this dag/steps template. Despite its
    /// name suggesting wide reach, Argo only uses this as the
    /// **boundary fallback** for pods whose IMMEDIATE enclosing
    /// dag/steps is this template (proven from
    /// `workflow/controller/workflowpod.go:928-958`). It does NOT
    /// cascade through nested sub-workflows — a `pipeline → sub →
    /// container` chain doesn't see `pipeline`'s selector on `container`.
    /// For "every pod in the run by default", use
    /// `node_selector_if_root` (below). Renamed 2026-05-24 from
    /// the misleading `node_selector` after a real e2e bug.
    pub(crate) boundary_node_selector: std::collections::BTreeMap<String, String>,
    /// `boundary_tolerations = [{ key, operator, value, effect, ... }, …]`
    /// — `Template.Tolerations` on this dag/steps template, inherited
    /// by child pods that don't set their own (Argo's 3-tier `tmpl →
    /// boundary → wfSpec` lookup at `workflow/controller/workflowpod
    /// .go:928-958`). Literal-only by construction: per-arg injection
    /// here would lower to `workflow.parameters` (root-scoped) and
    /// surprise-resolve against the SUBMITTED ROOT's args if this WT
    /// is `templateRef`'d as a sub — same rationale as
    /// `boundary_node_selector`. Use `tolerations_if_root` for
    /// dynamic values.
    pub(crate) boundary_tolerations: Vec<BoundaryTolerationArg>,
    /// `boundary_affinity = "<json|yaml>"` — `Template.Affinity` on
    /// this dag/steps template, opaque YAML/JSON. Inherited by child
    /// pods that don't set their own. Literal string only (same
    /// boundary-tier rationale as `boundary_tolerations`); hand-write
    /// `{{...}}` inside the YAML if you really need substitution and
    /// understand the boundary-copy semantics.
    pub(crate) boundary_affinity: Option<String>,
    /// `node_selector_if_root = { "k" = "v", "k2" = "lit" + arg, … }` —
    /// Argo `WorkflowSpec.NodeSelector`. The third tier of Argo's 3-tier
    /// pod nodeSelector lookup: applies to every pod in the run that
    /// doesn't have its own template-level or boundary-level selector
    /// set. Root-only (same family as `ttl_if_root`/`pod_gc_if_root`/
    /// `active_deadline_if_root`); inert when this WT is `templateRef`'d
    /// as a sub-workflow. Values support the same `"lit" + arg` /
    /// `"lit" + arg.field` injection grammar as `#[container]` attrs,
    /// but lower to `{{=fromJSON(workflow.parameters['arg'])}}` — i.e.
    /// the SUBMITTED ROOT workflow's `arguments.parameters` (always
    /// root-scoped, never this template's inputs.parameters; the latter
    /// is empirically inert at WorkflowSpec scope on v4.0.5). Keys are
    /// literal-only.
    pub(crate) node_selector_if_root: std::collections::BTreeMap<String, syn::Expr>,
    pub(crate) on_exit_if_root: Option<syn::Path>,
    /// Template-level `retryStrategy` (`limit` required when present).
    pub(crate) retry: Option<RetryArgs>,
    /// Root-only WorkflowSpec TTL GC (`ttl_if_root(after_completion=…)`).
    pub(crate) ttl_if_root: Option<TtlArgs>,
    /// Root-only WorkflowSpec pod GC (`pod_gc_if_root(strategy=…)`).
    pub(crate) pod_gc_if_root: Option<PodGcArgs>,
    /// Root-only whole-workflow runtime cap →
    /// `WorkflowSpec.activeDeadlineSeconds` (`active_deadline_if_root =
    /// <secs | "2h">`). `timeout`/`pod_running_timeout` are
    /// `#[container]`-only (Argo no-ops on dag/steps), so this is the
    /// only way to time-bound a `#[workflow]`.
    pub(crate) active_deadline_if_root: Option<syn::Expr>,
    /// `annotations = { "key" = "value" }` — template-level annotations
    /// on the dag/steps template. **Literal strings only** (keys *and*
    /// values), same reason as `#[workflow(node_selector)]`: a workflow
    /// has no args to inject from, and template-scoped templating
    /// doesn't cascade. Drop in `{{workflow.parameters.X}}` as a
    /// literal value if you need a dynamic annotation.
    pub(crate) annotations: std::collections::BTreeMap<String, String>,
    /// `mutexes = [{ name = "x", namespace = "ns" }, …]` — template-level
    /// `Template.synchronization.mutexes` on this dag/steps template.
    /// See `ContainerArgs::mutexes`; same shape, same injection scope
    /// (`inputs.parameters`).
    pub(crate) mutexes: Vec<MutexArg>,
    /// `mutexes_if_root = [{ name = "x", namespace = "ns" }, …]` —
    /// root-only `WorkflowSpec.synchronization.mutexes`. See
    /// `ContainerArgs::mutexes_if_root`; same shape, same injection
    /// scope (`workflow.parameters`).
    pub(crate) mutexes_if_root: Vec<MutexArg>,
    /// `tolerations_if_root = [...]` → root-only `WorkflowSpec
    /// .Tolerations`. See `ContainerArgs::tolerations_if_root`; same
    /// shape, same injection scope.
    pub(crate) tolerations_if_root: Vec<TolerationArg>,
    /// `affinity_if_root = "<json|yaml>"` → root-only `WorkflowSpec
    /// .Affinity` as an opaque YAML/JSON string. See
    /// `ContainerArgs::affinity_if_root`.
    pub(crate) affinity_if_root: Option<syn::Expr>,
    /// `pod_spec_patch_if_root = "<json|yaml>"` — root-only
    /// `WorkflowSpec.PodSpecPatch`. Applied to every pod in the run
    /// (Argo concats this with each template's own `pod_spec_patch`
    /// before strategic-merging onto the rendered pod). String
    /// accepts the `"lit" + arg + arg.field` injection grammar;
    /// operands lower to `{{=fromJSON(workflow.parameters[..])}}`,
    /// the only scope Argo resolves at WorkflowSpec.
    pub(crate) pod_spec_patch_if_root: Option<syn::Expr>,
    /// `image_pull_secrets_if_root = ["regcred", ...]` — root-only
    /// `WorkflowSpec.ImagePullSecrets`. See `ContainerArgs::image_pull
    /// _secrets_if_root`; same shape (literal Secret names, no
    /// injection).
    pub(crate) image_pull_secrets_if_root: Vec<String>,
    /// `parallelism = N` — `Template.parallelism` on this dag/steps
    /// template. Caps concurrent children scheduled under THIS
    /// template invocation (pods from nested templates don't count).
    /// Literal `i64` only (Argo's `*int64` schema rejects substituted
    /// strings at admission). Argo CRD enforces `Minimum=1`; the
    /// macro rejects `<= 0` with a spanned compile_error.
    pub(crate) parallelism: Option<i64>,
    /// `parallelism_if_root = N` — root-only `WorkflowSpec.parallelism`.
    /// Caps total concurrent pods across the run; inert when this WT
    /// is `templateRef`'d. Same literal-only / `> 0` constraints as
    /// `parallelism`.
    pub(crate) parallelism_if_root: Option<i64>,
}

/// Parse attribute args into `T`, or return a `compile_error!`.
pub(crate) fn parse_attr<T: deluxe::ParseMetaItem + Default>(
    attr: TokenStream,
) -> Result<T, TokenStream> {
    if attr.is_empty() {
        return Ok(T::default());
    }
    deluxe::parse2::<T>(attr.into()).map_err(|e| e.into_compile_error().into())
}

/// Lower `retry(..)` to a token expr of type
/// `::core::option::Option<::cargo_athena::api::RetryStrategy>`.
/// `limit` is required; `limit = unlimited` (bare ident) ⇒ nil limit
/// (Argo treats that as unbounded); `policy` (if any) ∈ the 4 Argo
/// policies; `backoff` ⇒ `Backoff { duration }`.
pub(crate) fn retry_strategy_tokens(
    retry: &Option<RetryArgs>,
    span: proc_macro2::Span,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    let r = match retry {
        None => return Ok(quote! { ::core::option::Option::None }),
        Some(r) => r,
    };
    // `limit` REQUIRED inside `retry(..)`.
    let limit_tok = match &r.limit {
        None => {
            return Err(syn::Error::new(
                span,
                "`retry(...)` requires `limit = N` (or `limit = unlimited`)",
            ));
        }
        Some(Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        })) => {
            // Parse straight to the wire type (Argo's limit is an i32):
            // an out-of-range literal is a spanned error, not a silent
            // wrap to a negative limit.
            let n: i32 = i.base10_parse()?;
            quote! { ::core::option::Option::Some(#n) }
        }
        Some(Expr::Path(p)) if p.path.is_ident("unlimited") => {
            quote! { ::core::option::Option::None }
        }
        Some(e) => {
            return Err(syn::Error::new_spanned(
                e,
                "`limit` must be an integer or `unlimited`",
            ));
        }
    };
    // `policy` ∈ the 4 Argo retry policies (absent ⇒ Argo default).
    let policy_tok = match &r.policy {
        None => quote! { ::std::string::String::new() },
        Some(p) => {
            match p.value().as_str() {
                "Always" | "OnFailure" | "OnError" | "OnTransientError" => {}
                other => {
                    return Err(syn::Error::new_spanned(
                        p,
                        format!(
                            "unknown retry policy `{other}` (expected \
                             Always|OnFailure|OnError|OnTransientError)"
                        ),
                    ));
                }
            }
            quote! { #p.to_string() }
        }
    };
    let backoff_tok = match parse_opt_duration_secs(&r.backoff, "retry(backoff)")? {
        None => quote! { ::core::option::Option::None },
        Some(secs) => {
            // Argo `Backoff.Duration` is a Go-duration string; emit
            // canonical `"<n>s"` so humantime days/weeks normalize too.
            let dur = format!("{secs}s");
            quote! {
                ::core::option::Option::Some(::cargo_athena::api::Backoff {
                    duration: #dur.to_string(),
                    ..::core::default::Default::default()
                })
            }
        }
    };
    Ok(quote! {
        ::core::option::Option::Some(::cargo_athena::api::RetryStrategy {
            limit: #limit_tok,
            retry_policy: #policy_tok,
            backoff: #backoff_tok,
        })
    })
}

/// The one duration parser shared by **every** duration-bearing
/// attribute (`timeout`, `pod_running_timeout`, `active_deadline_if_root`,
/// `ttl_if_root(..)`, `retry(backoff)`) so the accepted syntax is
/// uniform everywhere. An **integer literal = whole seconds**; a
/// **string literal = a [`humantime`] duration** (`"90s"`, `"1h30m"`,
/// `"2d"`). Returns whole seconds, enforced `> 0`; a sub-second
/// component is a targeted error (Argo's fields count whole seconds,
/// and silently truncating `"1h500ms"` would lie about the emitted
/// value). Errors span the offending value expression. `attr` names
/// the attribute in diagnostics.
pub(crate) fn parse_duration_secs(e: &Expr, attr: &str) -> Result<u64, syn::Error> {
    let bad = || {
        syn::Error::new_spanned(
            e,
            format!(
                "`{attr}`: expected a positive integer (seconds) or a \
                 duration string like \"1h30m\" / \"2d\" (humantime)"
            ),
        )
    };
    let secs: u64 = match e {
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Int(i),
            ..
        }) => i.base10_parse::<u64>().map_err(|_| bad())?,
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => {
            let d = humantime::parse_duration(&s.value()).map_err(|_| bad())?;
            if d.subsec_nanos() != 0 {
                return Err(syn::Error::new_spanned(
                    e,
                    format!(
                        "`{attr}`: sub-second durations are not supported \
                         (Argo counts whole seconds, so {:?} cannot be \
                         represented exactly) - use whole seconds",
                        s.value()
                    ),
                ));
            }
            d.as_secs()
        }
        _ => {
            return Err(syn::Error::new_spanned(
                e,
                format!("`{attr}`: expected an integer or a duration string"),
            ));
        }
    };
    if secs == 0 {
        return Err(bad());
    }
    Ok(secs)
}

/// `None` → `Ok(None)`; else `parse_duration_secs`.
pub(crate) fn parse_opt_duration_secs(
    e: &Option<Expr>,
    attr: &str,
) -> Result<Option<u64>, syn::Error> {
    match e {
        None => Ok(None),
        Some(e) => Ok(Some(parse_duration_secs(e, attr)?)),
    }
}

/// Whole seconds → an `Option<i32>` token (Argo int-seconds fields:
/// `Template.activeDeadlineSeconds`, `ttlStrategy.secondsAfter*`).
pub(crate) fn secs_i32_tok(
    e: &Option<Expr>,
    attr: &str,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    match e {
        None => Ok(quote! { ::core::option::Option::None }),
        Some(expr) => match parse_duration_secs(expr, attr)? {
            s if s <= i32::MAX as u64 => {
                let n = s as i32;
                Ok(quote! { ::core::option::Option::Some(#n) })
            }
            _ => Err(syn::Error::new_spanned(
                expr,
                format!("`{attr}`: duration is too large (max ~68 years)"),
            )),
        },
    }
}

/// Whole seconds → an `Option<i64>` token (Argo
/// `WorkflowSpec.activeDeadlineSeconds`, an `int64`).
pub(crate) fn secs_i64_tok(
    e: &Option<Expr>,
    attr: &str,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    match e {
        None => Ok(quote! { ::core::option::Option::None }),
        Some(expr) => match parse_duration_secs(expr, attr)? {
            s if s <= i64::MAX as u64 => {
                let n = s as i64;
                Ok(quote! { ::core::option::Option::Some(#n) })
            }
            _ => Err(syn::Error::new_spanned(
                expr,
                format!("`{attr}`: duration is too large"),
            )),
        },
    }
}

/// Lower a literal `parallelism` / `parallelism_if_root = N` attr to
/// an `Option<i64>` token. Argo's CRD enforces `Minimum=1` on both
/// `Template.parallelism` and `WorkflowSpec.parallelism`, and the
/// `*int64` field rejects substituted strings at admission — so the
/// attr is literal-only and `<= 0` is a spanned compile error.
pub(crate) fn parallelism_tok(
    v: Option<i64>,
    span: proc_macro2::Span,
    attr: &str,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    match v {
        None => Ok(quote! { ::core::option::Option::None }),
        Some(n) if n > 0 => Ok(quote! { ::core::option::Option::Some(#n) }),
        Some(_) => Err(syn::Error::new(
            span,
            format!(
                "`{attr}` must be > 0 (Argo's CRD enforces Minimum=1; \
                 omit the attr to leave the field unset)"
            ),
        )),
    }
}

/// Lower the container `timeout = <secs | "1h30m">` attr to a `String`
/// token (Argo `Template.timeout`, a Go `time.ParseDuration` string).
/// We emit canonical `"<n>s"` — Go always accepts it, and humantime
/// days/weeks normalize to seconds so `"2d"` works even though Go has
/// no day unit. `None` ⇒ empty string (skip-serialized field default).
pub(crate) fn timeout_tokens(e: &Option<Expr>) -> Result<proc_macro2::TokenStream, syn::Error> {
    match parse_opt_duration_secs(e, "timeout")? {
        None => Ok(quote! { ::std::string::String::new() }),
        Some(s) => {
            let v = format!("{s}s");
            Ok(quote! { #v.to_string() })
        }
    }
}

/// Lower `ttl_if_root(..)` to a token expr of type
/// `::core::option::Option<::cargo_athena::api::TtlStrategy>`. ≥1 of the
/// three bounds is required (an empty `ttl_if_root()` is a compile error).
pub(crate) fn ttl_const_tokens(
    ttl: &Option<TtlArgs>,
    span: proc_macro2::Span,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    let t = match ttl {
        None => return Ok(quote! { ::core::option::Option::None }),
        Some(t) => t,
    };
    if t.after_completion.is_none() && t.after_success.is_none() && t.after_failure.is_none() {
        return Err(syn::Error::new(
            span,
            "`ttl_if_root(...)` needs at least one of \
             after_completion/after_success/after_failure",
        ));
    }
    let comp = secs_i32_tok(&t.after_completion, "ttl_if_root(after_completion)")?;
    let succ = secs_i32_tok(&t.after_success, "ttl_if_root(after_success)")?;
    let fail = secs_i32_tok(&t.after_failure, "ttl_if_root(after_failure)")?;
    Ok(quote! {
        ::core::option::Option::Some(::cargo_athena::api::TtlStrategy {
            seconds_after_completion: #comp,
            seconds_after_success: #succ,
            seconds_after_failure: #fail,
        })
    })
}

/// Lower `pod_gc_if_root(strategy = "<S>")` to a token expr of type
/// `::core::option::Option<&'static str>`. `strategy` is required and
/// must be one of the four Argo podGC strategies.
pub(crate) fn pod_gc_const_tokens(
    pod_gc: &Option<PodGcArgs>,
    span: proc_macro2::Span,
) -> Result<proc_macro2::TokenStream, syn::Error> {
    let g = match pod_gc {
        None => return Ok(quote! { ::core::option::Option::None }),
        Some(g) => g,
    };
    let s = match &g.strategy {
        None => {
            return Err(syn::Error::new(
                span,
                "`pod_gc_if_root(...)` requires `strategy = \"...\"`",
            ));
        }
        Some(s) => s,
    };
    match s.value().as_str() {
        "OnPodCompletion" | "OnPodSuccess" | "OnWorkflowCompletion" | "OnWorkflowSuccess" => {}
        other => {
            return Err(syn::Error::new_spanned(
                s,
                format!(
                    "unknown podGC strategy `{other}` (expected \
                     OnPodCompletion|OnPodSuccess|OnWorkflowCompletion|OnWorkflowSuccess)"
                ),
            ));
        }
    }
    Ok(quote! { ::core::option::Option::Some(#s) })
}

pub(crate) const UNSUPPORTED_INJECT: &str = "unsupported #[container] attribute value. \
Use a string literal, or a `+`-concatenation of string literals and \
container arguments / their named fields — e.g. `\"repo:\" + tag` or \
`\"repo:\" + meta.id + \"-x\"`. Method calls, other idents, tuple/index \
fields, and other expressions aren't supported.";

/// Lower a `#[container]`/`#[workflow]` attribute value to an Argo
/// string. A lone string literal is verbatim (so a hand-written `{{…}}`
/// passes through untouched — the power-user escape hatch). A
/// `+`-concatenation lowers each `arg` / `arg.named.field` operand to
/// `{{=fromJSON(<scope>['arg'](['f'])*)}}` (the raw value — no outer
/// `toJSON`, since this injects into an Argo-native string field, not
/// athena's run-side), literal segments verbatim.
///
/// `scope` is `"inputs.parameters"` for `#[container]` attrs (per-pod,
/// substituted by `workflowpod.go:106` ProcessArgs against the
/// container's own inputs) or `"workflow.parameters"` for
/// `#[workflow]` attrs (root-scoped, substituted from the SUBMITTED
/// root's `arguments.parameters` — empirically proven on v4.0.5 to
/// resolve at both `Template.NodeSelector` and `WorkflowSpec
/// .NodeSelector`, whereas `inputs.parameters` does NOT — the boundary
/// fallback at `workflowpod.go:938` copies the raw template string
/// before any per-template substitution can resolve it). `kind` is the
/// macro name to thread into error messages.
///
/// Injected operands are recorded for the `Injectable` type-guard.
pub(crate) fn inject_lower(
    e: &Expr,
    args: &std::collections::HashSet<String>,
    ops: &mut Vec<Expr>,
    scope: &str,
    kind: &str,
) -> syn::Result<String> {
    match unwrap_expr(e) {
        Expr::Binary(b) if matches!(b.op, syn::BinOp::Add(_)) => {
            let mut s = inject_lower(&b.left, args, ops, scope, kind)?;
            s.push_str(&inject_lower(&b.right, args, ops, scope, kind)?);
            Ok(s)
        }
        Expr::Lit(syn::ExprLit {
            lit: syn::Lit::Str(s),
            ..
        }) => Ok(s.value()),
        Expr::Path(p) if p.path.segments.len() == 1 => {
            let id = p.path.segments[0].ident.to_string();
            if !args.contains(&id) {
                return Err(syn::Error::new_spanned(
                    e,
                    format!(
                        "`{id}` is not a parameter of this #[{kind}] — \
                         only its arguments can be injected."
                    ),
                ));
            }
            ops.push(unwrap_expr(e).clone());
            Ok(format!("{{{{=fromJSON({scope}['{id}'])}}}}"))
        }
        Expr::Field(_) => {
            let mut path: Vec<String> = Vec::new();
            let mut cur = unwrap_expr(e);
            while let Expr::Field(fe) = cur {
                match &fe.member {
                    syn::Member::Named(n) => path.push(n.to_string()),
                    syn::Member::Unnamed(_) => {
                        return Err(syn::Error::new_spanned(
                            fe,
                            "tuple-field access (`a.0`) can't be injected \
                             — named struct fields only.",
                        ));
                    }
                }
                cur = unwrap_expr(&fe.base);
            }
            path.reverse();
            let Expr::Path(p) = cur else {
                return Err(syn::Error::new_spanned(cur, UNSUPPORTED_INJECT));
            };
            if p.path.segments.len() != 1 {
                return Err(syn::Error::new_spanned(cur, UNSUPPORTED_INJECT));
            }
            let root = p.path.segments[0].ident.to_string();
            if !args.contains(&root) {
                return Err(syn::Error::new_spanned(
                    cur,
                    format!("`{root}` is not a parameter of this #[{kind}]."),
                ));
            }
            ops.push(unwrap_expr(e).clone());
            let acc: String = path.iter().map(|f| format!("['{f}']")).collect();
            Ok(format!("{{{{=fromJSON({scope}['{root}']){acc}}}}}"))
        }
        other => Err(syn::Error::new_spanned(other, UNSUPPORTED_INJECT)),
    }
}

/// Lower a `Vec<MutexArg>` to the resolved `(name, namespace)` string
/// pairs that the macro stamps into either `api::Template
/// .synchronization` (template-level) or the `MUTEXES_IF_ROOT` const
/// (root-level). Each element is run through `inject_lower` against
/// `scope` (`inputs.parameters` for template-level, `workflow.parameters`
/// for `_if_root`) so a literal string stays verbatim and a
/// `"lit" + arg` chain becomes a `{{=fromJSON(scope['arg'])}}` expr.
/// Empty `namespace` ⇒ `""`, which `argo!`'s skip-if-empty omits so
/// Argo falls back to the workflow's own namespace.
pub(crate) fn lower_mutex_pairs(
    list: &[MutexArg],
    args: &std::collections::HashSet<String>,
    ops: &mut Vec<syn::Expr>,
    scope: &str,
    kind: &str,
) -> syn::Result<Vec<(String, String)>> {
    list.iter()
        .map(|m| {
            let name = inject_lower(&m.name, args, ops, scope, kind)?;
            let ns = match &m.namespace {
                Some(e) => inject_lower(e, args, ops, scope, kind)?,
                None => String::new(),
            };
            Ok((name, ns))
        })
        .collect()
}

/// K8s `Toleration.operator` is a closed set; anything else fails at
/// k8s admission, so fail at compile time with the literal spanned.
pub(crate) fn check_toleration_operator(op: &syn::LitStr) -> syn::Result<()> {
    match op.value().as_str() {
        "Equal" | "Exists" => Ok(()),
        other => Err(syn::Error::new_spanned(
            op,
            format!("unknown toleration operator `{other}` (expected Equal|Exists)"),
        )),
    }
}

/// K8s `Toleration.effect` closed set (empty = tolerate every effect).
/// Checked on the *lowered* string so an injected value or a
/// hand-written `{{…}}` substitution (resolved by Argo at run time)
/// passes through unchecked.
pub(crate) fn check_toleration_effect<T: quote::ToTokens>(
    lowered: &str,
    tokens: &T,
) -> syn::Result<()> {
    if lowered.contains("{{") {
        return Ok(());
    }
    match lowered {
        "" | "NoSchedule" | "PreferNoSchedule" | "NoExecute" => Ok(()),
        other => Err(syn::Error::new_spanned(
            tokens,
            format!(
                "unknown toleration effect `{other}` (expected \
                 NoSchedule|PreferNoSchedule|NoExecute, or empty to match all)"
            ),
        )),
    }
}

/// Lower a `Vec<TolerationArg>` to `(key, operator, value, effect,
/// toleration_seconds)` 5-tuples. `key`/`value`/`effect` go through
/// `inject_lower` against the right scope (so `"lit" + arg` becomes
/// `{{=fromJSON(scope['arg'])}}`); `operator` is a literal string and
/// both closed sets are enforced here; `toleration_seconds` is a
/// literal i64.
/// 5-tuple matching `cargo_athena_core::TolerationTuple` — the lowered
/// shape the macro produces for each toleration entry.
pub(crate) type TolerationTuple = (String, String, String, String, i64);

pub(crate) fn lower_toleration_args(
    list: &[TolerationArg],
    args: &std::collections::HashSet<String>,
    ops: &mut Vec<syn::Expr>,
    scope: &str,
    kind: &str,
) -> syn::Result<Vec<TolerationTuple>> {
    list.iter()
        .map(|t| {
            check_toleration_operator(&t.operator)?;
            let key = inject_lower(&t.key, args, ops, scope, kind)?;
            let value = match &t.value {
                Some(e) => inject_lower(e, args, ops, scope, kind)?,
                None => String::new(),
            };
            let effect = inject_lower(&t.effect, args, ops, scope, kind)?;
            check_toleration_effect(&effect, &t.effect)?;
            Ok((key, t.operator.value(), value, effect, t.toleration_seconds))
        })
        .collect()
}

/// Const-tokens producer for `TOLERATIONS_IF_ROOT`. Each entry lowers
/// to a 5-tuple literal `(key, op, value, effect, secs)`.
pub(crate) fn tolerations_if_root_const_tokens(
    pairs: &[TolerationTuple],
) -> proc_macro2::TokenStream {
    let entries = pairs.iter().map(|(k, op, v, eff, secs)| {
        quote! { (#k, #op, #v, #eff, #secs) }
    });
    quote! { &[ #( #entries ),* ] }
}

/// Build a Template-level tolerations literal for a container or dag/steps
/// template. Returns tokens for a `Vec<api::Toleration>` expression
/// (constructed at runtime; empty vec skip-serializes).
pub(crate) fn template_tolerations_tokens(pairs: &[TolerationTuple]) -> proc_macro2::TokenStream {
    if pairs.is_empty() {
        return quote! { ::std::vec::Vec::new() };
    }
    let entries = pairs.iter().map(|(k, op, v, eff, secs)| {
        let secs_tok = if *secs == 0 {
            quote! { ::core::option::Option::None }
        } else {
            quote! { ::core::option::Option::Some(#secs) }
        };
        quote! {
            ::cargo_athena::api::Toleration {
                key: #k.to_string(),
                operator: #op.to_string(),
                value: #v.to_string(),
                effect: #eff.to_string(),
                toleration_seconds: #secs_tok,
            }
        }
    });
    quote! { ::std::vec![ #( #entries ),* ] }
}

/// Token producer for template-level mutexes: an
/// `Option<api::Synchronization>` expression inlined into the
/// `api::Template { synchronization: …, ... }` literal each macro emits
/// from `build()`. Empty list ⇒ `None` (skip-serialized, byte-identical
/// goldens for templates without mutexes).
pub(crate) fn template_synchronization_tokens(
    pairs: &[(String, String)],
) -> proc_macro2::TokenStream {
    if pairs.is_empty() {
        return quote! { ::core::option::Option::None };
    }
    let muts = pairs.iter().map(|(n, ns)| {
        quote! {
            ::cargo_athena::api::Mutex {
                name: #n.to_string(),
                namespace: #ns.to_string(),
            }
        }
    });
    quote! {
        ::core::option::Option::Some(::cargo_athena::api::Synchronization {
            mutexes: ::std::vec![ #( #muts ),* ],
        })
    }
}

/// Token producer for the `MUTEXES_IF_ROOT` trait const — the
/// `&'static [(&'static str, &'static str)]` array the `Collector`
/// then stamps onto each declaring template's own
/// `spec.synchronization.mutexes` in `stamp_spec`. Empty list ⇒ `&[]`.
pub(crate) fn mutexes_if_root_const_tokens(pairs: &[(String, String)]) -> proc_macro2::TokenStream {
    let names: Vec<&String> = pairs.iter().map(|(n, _)| n).collect();
    let nss: Vec<&String> = pairs.iter().map(|(_, ns)| ns).collect();
    quote! {
        &[ #( (#names, #nss) ),* ]
    }
}
