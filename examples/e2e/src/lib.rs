//! The kind e2e fixture (`scripts/e2e-test.sh`,
//! `.github/workflows/e2e.yml`) — the **only** crate submitted to a
//! real Argo + MinIO, across 3 Argo versions.
//!
//! One mega `#[workflow]` exercises the full feature matrix end-to-end
//! so latent real-Argo bugs surface in CI, not by hand:
//!
//! * container→container **param data-deps**
//!   (`{{tasks.x.outputs.parameters.return}}` — run-mode (de)serialize,
//!   `ATHENA_PARAM_*` in, `/athena/result` out),
//! * a nested `#[workflow]` that **returns a value** consumed downstream
//!   (workflow→container across the `templateRef` wormhole),
//! * **`.fan_out`** → Argo `withParam`, aggregate consumed as `Vec`,
//! * **value-`if`** + **statement `if`/`else`** (synthesized `when`
//!   wrappers) with a closed-grammar numeric condition,
//! * a **nested call** `note(echo(..))`,
//! * **struct-field access** `m.id` (`{{=toJSON(fromJSON(..)['id'])}}`),
//! * **attribute injection** `image = "busybox:" + tag` (resolves to a
//!   real pullable tag),
//! * **`save_artifact!` + `load_artifact!`** round-trip through MinIO
//!   (ordered by a threaded data-dep),
//! * **per-task builders** `.continue_on` + `.on_exit`,
//! * **`#[workflow(node_selector=…)]`** (cascades onto every pod) +
//!   **`on_exit_if_root`** (whole-workflow exit handler),
//! * default image (busybox) + explicit `#[container(image=…)]`,
//! * `host!` + a `#[fragment]` carrying its own `host!`,
//! * binary delivery: cross-musl tarball in MinIO, `uname` bootstrap,
//!   scheduled on worker nodes.
//!
//! Every image is **busybox:1.36-musl-pullable** (default, explicit, or
//! the injected `busybox:<tag>`) so it actually runs on the cluster.

use cargo_athena::{container, fragment, workflow};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct Meta {
    pub id: String,
    pub n: i64,
}

#[container]
pub fn produce() -> String {
    "hello".to_string()
}

#[container(image = "busybox:1.36-musl")]
pub fn transform(input: String) -> String {
    format!("{input}-transformed")
}

#[fragment]
fn extra_mount() {
    let _ = cargo_athena::host!("/tmp/athena-frag");
}

#[container]
pub fn consume(value: String) {
    let h = cargo_athena::host!("/tmp/athena-host");
    extra_mount();
    println!("consume({value}) host={h}");
    cargo_athena::save_artifact_str!("result-note", format!("done:{value}"));
}

#[container]
pub fn stamp() -> String {
    "stamped".to_string()
}

#[container]
pub fn audit(msg: String) {
    println!("audit:{msg}");
}

#[container]
pub fn report(summary: String) {
    println!("report:{summary}");
}

/// Nested workflow that RETURNS a value (tail call's `return` becomes
/// this template's own `outputs.parameters.return`).
#[workflow]
pub fn finalize_wf() -> String {
    stamp()
}

// --- nested call -----------------------------------------------------------

#[container]
pub fn echo(s: String) -> String {
    format!("echo:{s}")
}

#[container]
pub fn note(msg: String) {
    println!("note:{msg}");
}

// --- fan-out ---------------------------------------------------------------

#[container]
pub fn make_list() -> Vec<String> {
    vec!["a".to_string(), "b".to_string(), "c".to_string()]
}

#[container]
pub fn upper(s: String) -> String {
    s.to_uppercase()
}

#[container]
pub fn summarize(items: Vec<String>) -> String {
    format!("n={};{}", items.len(), items.join(","))
}

// --- conditionals ----------------------------------------------------------

#[container]
pub fn count(s: String) -> i64 {
    s.len() as i64
}

#[container]
pub fn left(x: i64) -> String {
    format!("L{x}")
}

#[container]
pub fn right(x: i64) -> String {
    format!("R{x}")
}

// --- struct-field access ---------------------------------------------------

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

// --- attribute injection (image = "busybox:" + tag) ------------------------

#[container(image = "busybox:" + tag)]
pub fn tagged(tag: String) {
    println!("tagged image busybox:{tag}");
}

// --- artifact ports (save -> load, ordered by a threaded dep) --------------

#[container]
pub fn seed_note() -> String {
    cargo_athena::save_artifact_str!("e2e-note", "payload-42".to_string());
    "seeded".to_string()
}

#[container]
pub fn read_note(_after: String) {
    let v = cargo_athena::load_artifact_str!("e2e-note");
    println!("read e2e-note = {v}");
}

// --- per-task builders + the exit handler ----------------------------------

#[container]
pub fn risky() -> String {
    "ok".to_string()
}

#[container]
pub fn finalize(r: String) {
    println!("finalize:{r}");
}

/// `on_exit_if_root` handler (whole-workflow) AND a per-task
/// `.on_exit(cleanup)` target — both should run.
#[container]
pub fn cleanup() {
    println!("cleanup ran");
}

/// The whole feature matrix in one DAG. `node_selector` cascades onto
/// every task pod; `on_exit_if_root` fires `cleanup` once at the end
/// (only because *this* is the submitted root).
#[workflow(
    node_selector = { "kubernetes.io/arch" = "amd64" },
    on_exit_if_root = cleanup,
)]
pub fn pipeline() {
    let a = produce();
    let b = transform(a); // container -> container param dep
    consume(b);

    let s = finalize_wf(); // nested workflow returns a value
    audit(s); // workflow -> container dep

    note(echo("nested".to_string())); // nested call: echo then note(dep)

    let items = make_list();
    let caps = items.fan_out(|x| upper(x)); // withParam
    let sum = summarize(caps); // aggregate Vec consumed
    report(sum);

    let cnt = count("abcd".to_string());
    let chosen = if cnt > 2 { left(cnt) } else { right(cnt) }; // value-if
    if cnt == 0 {
        note("zero".to_string());
    } else {
        note(chosen); // statement-if
    }

    let m = make_meta();
    use_id(m.id); // struct-field access

    tagged("1.36-musl".to_string()); // image = "busybox:" + tag

    let k = seed_note(); // save_artifact!
    read_note(k); // load_artifact!, ordered after seed_note via `k`

    let r = risky().continue_on(failed, error);
    finalize(r).on_exit(cleanup); // per-task exit hook
}
