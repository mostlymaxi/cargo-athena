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

/// `#[workflow(on_exit = t)]` → the runnable Workflow's `spec.onExit`
/// (only when this is the emit root). `.on_exit(record("done"))` → an
/// exit hook *with arguments* (resolved like task args).
#[workflow(on_exit = teardown)]
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
