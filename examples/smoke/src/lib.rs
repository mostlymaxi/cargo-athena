//! Broad-coverage "all features" fixture (library form, so it can be
//! imported by other crates). Exercises, in one place:
//!
//! * root `#[workflow]` with a multi-dependency DAG task,
//! * nested `#[workflow]` (workflow-calls-workflow) with an input-param ref,
//! * `#[workflow]` that **returns a value** consumed downstream
//!   (`sub_pipeline` / `pipeline_returns`),
//! * `#[container]` with explicit `image`/`bin` and with defaults,
//! * mixed arg kinds: string/int literals, `.to_string()`, input refs,
//!   prior-task refs,
//! * `host!` declared across BOTH `if/else` and `match` arms (static union),
//! * transitive `#[fragment]` resource closure (`frag_a` -> `frag_b`),
//! * native artifact ports (`load_artifact_str!`/`save_artifact!`).
//!
//! Every `#[workflow]`/`#[container]` is a `pub` type so downstream crates
//! (see `examples/importing`, which imports `pipeline` cross-crate) can
//! compose it. Keep this file stable; refresh goldens with
//! `UPDATE_EXPECT=1`.

use cargo_athena::{container, fragment, workflow};

// --- root workflow ---------------------------------------------------------

#[workflow]
pub fn pipeline() {
    let a = ingest("https://example.com/data".to_string()); // nested workflow
    branchy("fast".to_string()); // container, literal arg
    let t = transform("seed".to_string(), 3); // container, str + int literals
    combine(a, t); // multi-dependency DAG task: depends on `a` AND `t`
}

// --- nested workflow -------------------------------------------------------

#[workflow]
pub fn ingest(source: String) -> String {
    let raw = fetch(source); // `source` -> {{inputs.parameters.source}}
    let clean = transform(raw, 2); // raw -> task ref; 2 -> literal
    publish(clean.clone()); // `clean` fans out (publish + the return);
    clean // explicit `.clone()` == Argo copying the param to each consumer
}

// --- containers ------------------------------------------------------------

#[container(image = "ghcr.io/acme/fetch:1.2.3")]
pub fn fetch(url: String) -> String {
    let _token = cargo_athena::host!("/secrets/token");
    format!("data-from:{url}")
}

#[container] // default image (REPLACE_ME) + default bin (app)
pub fn transform(data: String, factor: i64) -> String {
    format!("{data}*{factor}")
}

#[container(image = "ghcr.io/acme/tools:latest")]
pub fn branchy(mode: String) {
    // host! collected from BOTH branches even though only one runs.
    if mode == "fast" {
        let _ = cargo_athena::host!("/cache/fast");
    } else {
        let _ = cargo_athena::host!("/cache/slow");
    }
    // ...and from every match arm.
    let _ = match mode.len() {
        0 => cargo_athena::host!("/data/empty"),
        _ => cargo_athena::host!("/data/default"),
    };
    frag_a(); // pulls /var/lib/a and (transitively) /var/lib/b
    println!("branchy mode={mode}");
}

// Param injection: literal `+` arg in `image` / `service_account`, and
// a literal node_selector key with an injected value. Keys stay literal.
#[container(
    image = "ghcr.io/acme/combine:" + rhs,
    service_account = "athena-" + lhs + "-runner",
    node_selector = { "kubernetes.io/arch" = "amd64", "disktype" = rhs }
)]
pub fn combine(lhs: String, rhs: String) -> String {
    format!("{lhs}+{rhs}")
}

#[container]
pub fn publish(report: String) {
    // Native Argo artifact ports (no S3): an input port read at runtime
    // and an output port written at runtime — both declared on this
    // container's WorkflowTemplate by static collection.
    let notes = cargo_athena::load_artifact_str!("notes");
    println!("publishing {report} (notes: {notes})");
    cargo_athena::save_artifact!("receipt", format!("ok:{report}"));
}

// --- fragments (cross-item resource carriers) ------------------------------

#[fragment]
fn frag_a() {
    let _a = cargo_athena::host!("/var/lib/a");
    frag_b(); // transitive: frag_b's host! must also land on `branchy`
}

#[fragment]
fn frag_b() {
    let _b = cargo_athena::host!("/var/lib/b");
}

// --- workflow return values ------------------------------------------------

/// A nested `#[workflow]` that *returns* a value: the tail template call's
/// `result` is bubbled up as this workflow-template's own `outputs.result`.
#[workflow]
pub fn sub_pipeline(seed: String) -> String {
    let fetched = fetch(seed); // container -> String; `seed` is an input
    transform(fetched, 7) // tail call (no `;`) == this workflow's result
}

/// Consumes a sub-*workflow*'s return value. Proves workflow→X data deps:
/// `{{tasks.r.outputs.result}}` resolves only because `sub_pipeline` now
/// declares that output (it didn't before — workflows had no outputs).
#[workflow]
pub fn pipeline_returns() {
    let r = sub_pipeline("seed".to_string());
    publish(r);
}

// --- per-task builders: .continue_on / .hooks / .on_exit --------------------

#[container]
pub fn cleanup() {
    println!("cleanup");
}

#[container]
pub fn alarm() {
    println!("alarm");
}

/// `.continue_on(...)` lets dependents proceed on failure/error;
/// `.on_exit(t)` is the unconditional `exit` hook; `.on_failure(t)` /
/// `.on_success(t)` are typed phase predicates (athena generates the
/// Argo expression); `.hook_if("raw-expr" = t)` is the escape hatch.
/// Hook templates are force-linked + emitted via the wormhole.
#[workflow]
pub fn pipeline_hooks() {
    let raw = fetch("https://example.com".to_string()).continue_on(failed, error);
    transform(raw, 9)
        .on_exit(cleanup)
        .on_failure(alarm)
        .hook_if("workflow.status == 'Failed'" = alarm);
}

#[container]
pub fn teardown() {
    println!("teardown");
}

#[container]
pub fn record(tag: String) {
    println!("record {tag}");
}

/// `#[workflow(on_exit_if_root = t)]` → this workflow's own
/// `spec.hooks.exit` (Argo fires it only when this workflow is the one
/// submitted). The per-task `.on_exit(record("done"))` is a different,
/// always-fires task hook (here *with arguments*).
#[workflow(on_exit_if_root = teardown)]
pub fn pipeline_onexit() {
    let raw = fetch("https://example.com".to_string());
    transform(raw, 2).on_exit(record("done"));
}

// --- struct-field access (`a.field`) ---------------------------------------

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub id: String,
    pub n: i64,
}

#[container]
pub fn make_meta() -> Meta {
    Meta {
        id: "abc".to_string(),
        n: 7,
    }
}

#[container]
pub fn use_id(id: String) {
    println!("id={id}");
}

/// `g(a.field)` — the consumer is wired a single struct field, lowered
/// via `{{=toJSON(fromJSON(tasks['m'].outputs.parameters['return'])['id'])}}`.
/// The ghost has already type-checked that `Meta::id` exists and is the
/// `String` `use_id` expects.
#[workflow]
pub fn pipeline_fields() {
    let m = make_meta();
    use_id(m.id);
}

// --- fan-out (`.fan_out` -> Argo `withParam`) -------------------------------

#[container]
pub fn make_list() -> Vec<String> {
    vec!["a".to_string(), "b".to_string(), "c".to_string()]
}

#[container]
pub fn caps(s: String, suffix: String) -> String {
    format!("{}{suffix}", s.to_uppercase())
}

#[container]
pub fn summarize(items: Vec<String>) {
    println!("got {} items", items.len());
}

/// `a.fan_out(|x| caps(x, "!"))` → Argo `withParam` over `a`; `caps`
/// runs once per element (`{{item}}`), and `b` is the aggregated
/// `Vec<String>` consumed by `summarize`. The ghost type-checks the
/// element type, the closure, and that `b: Vec<String>`.
#[workflow]
pub fn pipeline_fanout() {
    let a = make_list();
    let b = a.fan_out(|x| caps(x, "!".to_string()));
    summarize(b);
}

// --- conditionals (`if`/`else`/`else if` -> Argo `when` wrappers) -----------

#[container]
pub fn decide(seed: String) -> i64 {
    seed.len() as i64
}

#[container]
pub fn left(x: i64) -> String {
    format!("L{x}")
}

#[container]
pub fn right(x: i64) -> String {
    format!("R{x}")
}

#[container]
pub fn note(msg: String) {
    println!("{msg}");
}

/// Real Rust `if` lowered to synthesized `when`-gated wrapper workflows:
///
/// * **value-`if`** — `let chosen = if n > 3 { left(n) } else { right(n) };`
///   becomes one wrapper whose `outputs.parameters.return` selects the
///   arm that ran (status-ternary); `chosen` is consumed downstream as a
///   normal returning-workflow ref.
/// * **statement `if`/`else if`/`else`** with mixed conditions: numeric
///   equality (`n == 0`), `.field` access (`m.id == "abc"`), and `&&`.
///
/// The ghost type-checks the conditions + both arms (and that the
/// value-`if` arms agree on `String`) as ordinary Rust.
#[workflow]
pub fn pipeline_if() {
    let cnt = decide("hello".to_string());
    let m = make_meta();
    let chosen = if cnt > 3 { left(cnt) } else { right(cnt) };
    if cnt == 0 {
        note("zero".to_string());
    } else if m.id == "abc" && cnt > 1 {
        note(chosen);
    } else {
        note("other".to_string());
    }
}

/// Nested template calls — a call in argument position and a call in a
/// condition:
///
/// * `note(left(decide("x")))` → `decide`, then `left` (dep `decide`),
///   then `note` (dep `left`) — recursive, the inner result wired as a
///   normal output ref.
/// * `if decide("y") > 1 { … }` → `decide` is hoisted to a parent task
///   (the condition is evaluated unconditionally, like Rust) and the
///   `if` wrapper gates on its output.
#[workflow]
pub fn pipeline_nested() {
    note(left(decide("xx".to_string())));
    if decide("yy".to_string()) > 1 {
        note("big".to_string());
    } else {
        note("small".to_string());
    }
}

// --- attribute param injection of a struct field ---------------------------

/// `image = "..." + m.id` injects a *named struct field* of an arg
/// (`m.id` is `String`, so `Injectable`); lowered to
/// `{{=fromJSON(inputs.parameters['m'])['id']}}`.
#[container(image = "ghcr.io/acme/m:" + m.id)]
pub fn tag_meta(m: Meta) {
    println!("tagged {}", m.id);
}

#[workflow]
pub fn pipeline_inject() {
    let m = make_meta();
    tag_meta(m);
}

// --- #[workflow] boundary_node_selector + node_selector_if_root ------------

/// Two distinct nodeSelector knobs because Argo's pod-creation
/// fallback is 3-tier (`tmpl → boundary → wfSpec`; never walks the
/// ancestor chain):
///
/// * `boundary_node_selector` → `Template.NodeSelector` on this
///   dag/steps. **Literal-only.** Reaches only pods whose IMMEDIATE
///   enclosing dag/steps is this template (does NOT cascade through
///   nested sub-workflows). Dynamic values would have to lower to
///   `workflow.parameters` (root-scoped), which would silently
///   resolve against the SUBMITTED ROOT's args even when this WT is
///   `templateRef`'d — a footgun. Keep these static.
/// * `node_selector_if_root` → `WorkflowSpec.NodeSelector`. Root-only
///   default for every pod that doesn't have a tmpl- or boundary-level
///   override (inert when this WT is `templateRef`'d as a sub —
///   verified live on v4.0.5: the sub's spec.nodeSelector field isn't
///   even read in that path). Supports `"lit" + arg` /
///   `"lit" + arg.field` injection that lowers to
///   `{{=fromJSON(workflow.parameters['arg'])}}`, the ONLY form Argo
///   resolves at WorkflowSpec scope (proven on v4.0.5;
///   `inputs.parameters` is empirically inert here).
///
/// `boundary_node_selector` also accepts a raw `{{workflow.parameters
/// .region}}` literal as the documented eyes-open escape hatch for
/// dynamic values — owns its own root-scoping.
#[workflow(
    boundary_node_selector = {
        "kubernetes.io/arch" = "amd64",
        "topology.kubernetes.io/region" = "{{workflow.parameters.region}}",
    },
    node_selector_if_root = {
        "tier" = "platform",
        "env" = "prod-" + env,
    },
)]
pub fn pipeline_ns(env: String) {
    // `env` is consumed by the `node_selector_if_root` injection above
    // (via `workflow.parameters['env']`); the inject-check shim asserts
    // it's `Injectable` (`String` ⇒ raw-scalar fromJSON form).
    let raw = fetch("https://example.com".to_string());
    transform(raw, 3);
}

// --- #[container]/#[workflow] retry + container timeout -------------------

/// `retry(limit = N, policy = ..., backoff = <dur>)` + `timeout` lower
/// to template-level Argo `retryStrategy`/`timeout`. Every duration
/// (`backoff`, `timeout`) takes an int (seconds) or a humantime string
/// and is normalized to canonical `"<n>s"`. `timeout` is
/// `#[container]`-only (Argo no-op on dag/steps); `retry` is on both.
#[container(retry(limit = 3, policy = "OnError", backoff = "30s"), timeout = "5m")]
pub fn flaky() -> String {
    "ok".to_string()
}

/// `limit = unlimited` is the explicit opt-in for unbounded retries:
/// emits `retryStrategy` with **no** `limit` (Argo's nil = unlimited).
#[container(retry(limit = unlimited, policy = "Always"))]
pub fn flaky_forever() -> String {
    "ok".to_string()
}

#[workflow(retry(limit = 2))]
pub fn pipeline_retry() {
    flaky();
    flaky_forever();
}

// --- #[workflow] ttl + pod_gc (WorkflowSpec-scoped GC) --------------------

/// `ttl_if_root(after_completion=…, after_failure=…)` +
/// `pod_gc_if_root(strategy=…)` lower to the workflow's own
/// `spec.ttlStrategy` / `spec.podGC` — **root-only** (apply only when
/// this WT is the submitted workflow; same proven semantics as
/// `on_exit_if_root`).
#[workflow(
    ttl_if_root(after_completion = 86400, after_failure = 3600),
    pod_gc_if_root(strategy = "OnWorkflowSuccess")
)]
pub fn pipeline_ttl() {
    cleanup();
}

// --- timeouts: pod_running_timeout + active_deadline_if_root -------------

/// `pod_running_timeout = <secs | "1h30m">` → the pod's
/// `Template.activeDeadlineSeconds` (kubelet hard-kills the pod after
/// that long *running*). `#[container]`-only (Argo applies it only to
/// container/script templates). Int = seconds; string = humantime.
#[container(pod_running_timeout = 600)]
pub fn slowtask() {
    println!("slowtask");
}

/// Both timeouts on one container: `pod_running_timeout` →
/// `Template.activeDeadlineSeconds` (5400s); `active_deadline_if_root`
/// → this WT's own `WorkflowSpec.activeDeadlineSeconds` (3600s,
/// root-only — the genuine whole-workflow cap).
#[container(pod_running_timeout = "1h30m", active_deadline_if_root = 3600)]
pub fn slowtask2() {
    println!("slowtask2");
}

/// `active_deadline_if_root = "2h"` → this workflow's
/// `WorkflowSpec.activeDeadlineSeconds` (7200s). The only working
/// whole-workflow timeout — `timeout`/`pod_running_timeout` are Argo
/// no-ops on dag/steps, so they aren't accepted on `#[workflow]`.
#[workflow(active_deadline_if_root = "2h")]
pub fn pipeline_deadline() {
    slowtask();
    slowtask2();
}

// --- async fn #[container] -------------------------------------------------

/// `async fn` container — the macro wraps the body in
/// `cargo_athena::__async::block_on` (a single-thread tokio runtime
/// built per pod invocation). The emitted YAML is identical to a sync
/// container; only the in-pod execution path differs.
#[container]
pub async fn delay(label: String) -> String {
    cargo_athena::tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    format!("delayed:{label}")
}

#[workflow]
pub fn pipeline_async() {
    let _ = delay("hello".to_string());
}

// --- secret! / secret_opt! -------------------------------------------------

/// `#[fragment]` carrying a `secret!`. Every container that
/// transitively calls this gets a `valueFrom.secretKeyRef` env var
/// stamped on its template — same closure as `host!`/artifact ports.
#[fragment]
fn with_db_creds() {
    let _pw = cargo_athena::secret!("db-creds", "password");
}

/// Direct `secret!` + optional `secret_opt!` + fragment-carried
/// secret (`db-creds/password` propagated through `with_db_creds`).
#[container]
pub fn use_secrets(label: String) -> String {
    let token = cargo_athena::secret!("api-tokens", "api");
    let trace = cargo_athena::secret_opt!("debug-creds", "trace");
    with_db_creds(); // pulls db-creds/password onto this template
    format!("{label}:{token}:{trace:?}")
}

#[workflow]
pub fn pipeline_secrets() {
    use_secrets("hi".to_string());
}

// --- env / host_mount / annotations (pod-spec attrs) -----------------------

/// One container exercising all three new pod-spec attrs at once:
///
/// * `env = { K = "lit" + arg, … }` — extra env entries (literal keys,
///   injectable values). The body reads them via `std::env::var(…)`.
/// * `host_mount = [{ host_path, mount_path, read_only }, …]` — explicit
///   hostPath mounts with chosen mount paths (the safe-by-construction
///   `host!` always lands under `/athena/mounts/<munged>` instead).
/// * `annotations = { k = "lit" + arg, … }` — pod template annotations.
#[container(
    image = "ghcr.io/acme/svc:" + tag,
    privileged = true,
    env = {
        "LOG_LEVEL" = "info",
        "REGION"    = "us-" + zone,        // injected
    },
    host_mount = [
        { host_path = "/dev/shm", mount_path = "/dev/shm" },
        { host_path = "/etc/ca-certs", mount_path = "/certs", read_only = true },
    ],
    annotations = {
        "team.athena/owner" = "platform",
        "trace.athena/run"  = "run-" + tag,  // injected
    },
)]
pub fn run_svc(tag: String, zone: String) {
    println!("run_svc tag={tag} zone={zone}");
}

#[workflow(annotations = {
    "team.athena/owner" = "platform",
    "argo.io/wf-tier" = "{{workflow.parameters.tier}}",
})]
pub fn pipeline_pod_attrs() {
    run_svc("42".to_string(), "east".to_string());
}

// --- mutexes / mutexes_if_root ---------------------------------------------

/// Template-level `Template.synchronization.mutexes` (holder key
/// `<ns>/<wf>/<node>`, serializes across separate Workflow runs in the
/// same ns). `name` injects from the container's `shard` arg via
/// `inputs.parameters['shard']` — empirically safe on v4.0.5 (no
/// nodeSelector-style boundary-copy footgun at
/// `Template.synchronization`).
#[container(mutexes = [{ name = "shard-" + shard }])]
pub fn shard_writer(shard: String) {
    println!("writing shard {shard}");
}

/// Both tiers on one workflow:
///
/// * `mutexes = [{ name = "pipeline-dag" }]` — template-level on this
///   dag template (holder key `<ns>/<wf>/<node>`; literal name).
/// * `mutexes_if_root = [{ name = "global-deploy-" + env,
///   namespace = "ops-" + env }]` — root-only
///   `WorkflowSpec.synchronization.mutexes` (Argo's only whole-workflow
///   mutex; holder key `<ns>/<wf>`, inert when this WT is
///   `templateRef`'d). Both `name` and `namespace` inject from the
///   workflow's `env` arg via `workflow.parameters['env']` — the only
///   substitution scope Argo resolves at `WorkflowSpec`.
#[workflow(
    mutexes = [{ name = "pipeline-dag" }],
    mutexes_if_root = [
        { name = "global-deploy-" + env, namespace = "ops-" + env },
    ],
)]
pub fn pipeline_mutex(env: String) {
    shard_writer(env);
}

// --- Artifact<T> return-type: DAG-wired S3 passthrough ---------------------

/// `#[container]` whose return is `Artifact<Bag>` instead of plain
/// `Bag` -- the value flows via Argo's `outputs.artifacts.return` (S3-
/// backed, tar+gzip'd by the executor) rather than `outputs.parameters
/// .return` (inline in workflow status). Pins both producer-side
/// emission (`outputs.artifacts.return` with the templated `s3.key`
/// keyed off `{{pod.name}}`) and consumer-side wiring
/// (`arguments.artifacts.return.from: "{{tasks.<dep>.outputs.artifacts
/// .return}}"`). Lifts the parameter-size ceiling for large payloads.
#[container]
pub fn make_meta_artifact() -> cargo_athena::Artifact<Meta> {
    cargo_athena::Artifact::new(Meta {
        id: "abc".to_string(),
        n: 7,
    })
}

#[container]
pub fn use_meta_artifact(m: cargo_athena::Artifact<Meta>) {
    let meta = m.into_inner();
    println!("meta id={} n={}", meta.id, meta.n);
}

#[workflow]
pub fn pipeline_artifact() {
    let m = make_meta_artifact();
    use_meta_artifact(m);
}

/// Sub-workflow that *returns* `Artifact<Meta>` (bubbled from its
/// terminal container task) and another sub-workflow that *accepts*
/// `Artifact<Meta>` as an input (forwarded to a container). The root
/// `pipeline_artifact_wf` wires them via a parent dag task, exercising
/// the workflow-side `outputs.artifacts.return.from` bubble AND the
/// workflow-side `inputs.artifacts.<name>` forward, both across the
/// templateRef wormhole.
#[workflow]
pub fn sub_returning_artifact() -> cargo_athena::Artifact<Meta> {
    make_meta_artifact()
}

#[workflow]
pub fn sub_consuming_artifact(m: cargo_athena::Artifact<Meta>) {
    use_meta_artifact(m);
}

#[workflow]
pub fn pipeline_artifact_wf() {
    let m = sub_returning_artifact();
    sub_consuming_artifact(m);
}

/// `Artifact<T>` fan-out via `.clone()`: one producer (`make_meta
/// _artifact`) feeding two consumers. Same idiom as parameter-typed
/// bindings (clone is the explicit fan-out marker); the wire shape
/// just gives both consumers their own `arguments.artifacts.m.from:
/// '{{tasks.m.outputs.artifacts.return}}'` pointing at the same
/// upstream artifact. No in-pod clone happens; Argo downloads the
/// object once per consumer.
#[workflow]
pub fn pipeline_artifact_clone() {
    let m = make_meta_artifact();
    use_meta_artifact(m.clone());
    use_meta_artifact(m);
}

// --- tolerations + tolerations_if_root -------------------------------------

/// Template-level tolerations: `Template.Tolerations` on this
/// container's WT. `key` injects from the container's `kind` arg via
/// `{{=fromJSON(inputs.parameters['kind'])}}` (safe at template scope -
/// the leaf pod renders from this template's own substituted form, no
/// nodeSelector-style boundary-copy footgun). `operator` is literal
/// (small closed set); `effect` is also injectable.
#[container(tolerations = [
    { key = "athena.dev/" + kind, operator = "Exists", effect = "NoSchedule" },
])]
pub fn run_on_tainted_node(kind: String) {
    println!("ran on tainted node ({kind})");
}

/// Root-only `WorkflowSpec.Tolerations`: 3rd tier of Argo's
/// `tmpl → boundary → wfSpec` lookup, applies to every pod that
/// doesn't have a tighter override. Strings injectable via
/// `workflow.parameters` (the only scope Argo resolves at WfSpec).
#[workflow(tolerations_if_root = [
    { key = "athena.dev/" + role, operator = "Exists", effect = "NoSchedule" },
])]
pub fn pipeline_tolerations(role: String) {
    run_on_tainted_node(role);
}

// --- affinity + affinity_if_root -------------------------------------------

/// Template-level `Template.Affinity`: opaque YAML/JSON value. Athena
/// does NOT model `apiv1.Affinity`'s deeply-nested schema -- the user
/// owns the structure and athena parses + stuffs it verbatim at emit
/// time. Substitution at this scope is safe for the leaf pod.
/// `pod_spec_patch` is the alternative if you want patch-style.
#[container(affinity = r#"
nodeAffinity:
  preferredDuringSchedulingIgnoredDuringExecution:
    - weight: 1
      preference:
        matchExpressions:
          - key: kubernetes.io/arch
            operator: In
            values: [amd64]
"#)]
pub fn arch_pinned() {
    println!("preferred amd64");
}

/// Root-only `WorkflowSpec.Affinity`: opaque YAML/JSON, applies to
/// every pod in the run. The body can embed `{{workflow.parameters.X}}`
/// substitutions verbatim (Argo resolves at WfSpec scope).
#[workflow(affinity_if_root = r#"
nodeAffinity:
  requiredDuringSchedulingIgnoredDuringExecution:
    nodeSelectorTerms:
      - matchExpressions:
          - key: athena.dev/role
            operator: In
            values: ["{{workflow.parameters.role}}"]
"#)]
pub fn pipeline_affinity(role: String) {
    arch_pinned();
    // touch role so the macro doesn't error on an unused arg.
    record(role);
}

// --- pod_spec_patch / pod_spec_patch_if_root -------------------------------

/// Template-level `Template.PodSpecPatch` — the universal strategic-
/// merge escape hatch for any podSpec field athena hasn't lifted to
/// a first-class attr (here: resources). The patch string accepts the
/// `"lit" + arg` injection grammar; operands lower to
/// `{{=fromJSON(inputs.parameters[..])}}` and resolve at pod-creation
/// via `workflow/controller/workflowpod.go:89 processPodSpecPatch`
/// (no nodeSelector-style boundary-copy footgun — substitution
/// happens in the same pass that renders the patch onto the leaf pod;
/// proven v4.0.5 2026-05-26).
#[container(pod_spec_patch = r#"{"containers":[{"name":"main","resources":{"limits":{"cpu":""# + cpu_limit + r#"","memory":"64Mi"}}}]}"#)]
pub fn limited(cpu_limit: String) {
    println!("limited cpu={cpu_limit}");
}

/// Root-only `WorkflowSpec.PodSpecPatch` — Argo concats with each
/// template's own `pod_spec_patch` and applies the merge to every
/// pod in the run. Injection lowers to
/// `{{=fromJSON(workflow.parameters[..])}}` (the only scope Argo
/// resolves at WorkflowSpec). Inert when this WT is `templateRef`'d
/// — same `_if_root` family as `node_selector_if_root`.
#[workflow(
    pod_spec_patch_if_root = r#"{"terminationGracePeriodSeconds":"# + grace + r#"}"#,
)]
pub fn pipeline_pod_spec_patch(grace: String) {
    limited(grace);
}
