//! The kind e2e fixture (`scripts/e2e-test.sh`,
//! `.github/workflows/e2e.yml`) — the **only** crate submitted to a
//! real Argo + MinIO, across 3 Argo versions.
//!
//! One mega `#[workflow]` exercises the full feature matrix end-to-end
//! so latent real-Argo bugs surface in CI, not by hand:
//!
//! * container→container **param data-deps**
//!   (`{{tasks.x.outputs.parameters.return}}` — run-mode (de)serialize,
//!   positional argv in, `/athena/result` out),
//! * a nested `#[workflow]` that **returns a value** consumed downstream
//!   (workflow→container across the `templateRef` wormhole),
//! * **`.fan_out`** → Argo `withParam`, aggregate consumed as `Vec`,
//! * **value-`if`** + **statement `if`/`else`** (synthesized `when`
//!   wrappers) with a closed-grammar numeric condition,
//! * a **nested call** `note(echo(..))`,
//! * **struct-field access** `m.id` (`{{=toJSON(fromJSON(..)['id'])}}`),
//! * **attribute injection** on three leaf-template fields:
//!   `image = "busybox:" + tag` (real pullable tag),
//!   `service_account = "defa" + suffix` (resolves to `default` SA),
//!   `node_selector = { "kubernetes.io/arch" = arch }` (matches the
//!   kind node label). Exercises `Template.Image`/`.ServiceAccountName`/
//!   `.NodeSelector` substitution from `inputs.parameters` at the leaf,
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
    println!("consume({value}) host={}", h.display());
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

// Each fan_out consumer asserts the *exact* deserialized Rust value
// (not a stringified render) — a mis-encoded aggregate panics the pod
// → workflow Error → e2e red, with zero quote/format ambiguity.

#[container]
pub fn summarize(items: Vec<String>) {
    assert_eq!(
        items,
        vec!["A".to_string(), "B".to_string(), "C".to_string()],
        "fan_out String aggregate mis-decoded"
    );
    println!("OK summarize {items:?}");
}

/// Empty-source fan_out. `make_empty` yields `[]`, so Argo SKIPS the
/// `withParam` node (never aggregates) and fills the aggregate param
/// with `""`. Without the guard in the aggregate expr the pod would
/// hard-error on `fromJSON("")`; the fix must instead deliver an empty
/// list so `expect_empty` sees `vec![]`. Regression for the empty
/// fan_out bug.
#[container]
pub fn make_empty() -> Vec<String> {
    Vec::new()
}

#[container]
pub fn expect_empty(items: Vec<String>) {
    assert!(
        items.is_empty(),
        "empty fan_out must aggregate to [], got {items:?}"
    );
    println!("OK expect_empty {items:?}");
}

/// Nested element type — a struct holding a `Vec<i64>` ("a list of
/// json") to prove the `fan_out` aggregate re-normalization works
/// beyond flat `String` (Regime B writes exactly one layer per element
/// regardless of shape; if the aggregate double-encoded, deserializing
/// `Vec<Bag>` would ERROR, not silently mangle).
#[derive(Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct Bag {
    pub tag: String,
    pub vals: Vec<i64>,
}

fn bag(tag: &str, vals: Vec<i64>) -> Bag {
    Bag {
        tag: tag.to_string(),
        vals,
    }
}

#[container]
pub fn pack(tag: String) -> Bag {
    let n = tag.len() as i64;
    Bag {
        tag,
        vals: vec![n, n + 1, n + 2],
    }
}

#[container]
pub fn collect_bags(bags: Vec<Bag>) {
    assert_eq!(
        bags,
        vec![
            bag("a", vec![1, 2, 3]),
            bag("b", vec![1, 2, 3]),
            bag("c", vec![1, 2, 3]),
        ],
        "fan_out struct aggregate mis-decoded"
    );
    println!("OK collect_bags {bags:?}");
}

/// Varied-length source so the scalar element values differ.
#[container]
pub fn make_words() -> Vec<String> {
    vec!["x".to_string(), "yy".to_string(), "zzz".to_string()]
}

// --- fan_out element-type matrix (every row of the proven table) -----------
// scalar rows (Argo stringifies → string branch of the kind-aware expr):

#[container]
pub fn wlen(s: String) -> i64 {
    s.len() as i64 // 1, 2, 3
}

#[container]
pub fn sum_lens(ns: Vec<i64>) {
    assert_eq!(ns, vec![1_i64, 2, 3], "fan_out i64 aggregate mis-decoded");
    println!("OK sum_lens {ns:?}");
}

#[container]
pub fn is_even(s: String) -> bool {
    s.len().is_multiple_of(2) // false, true, false
}

#[container]
pub fn count_true(bs: Vec<bool>) {
    assert_eq!(
        bs,
        vec![false, true, false],
        "fan_out bool aggregate mis-decoded"
    );
    println!("OK count_true {bs:?}");
}

// structured row (Argo keeps native → pass-through branch):

#[container]
pub fn pair(s: String) -> Vec<Bag> {
    vec![
        Bag {
            tag: s.clone(),
            vals: vec![1],
        },
        Bag {
            tag: s,
            vals: vec![2],
        },
    ]
}

#[container]
pub fn flatten_bags(groups: Vec<Vec<Bag>>) {
    assert_eq!(
        groups,
        vec![
            vec![bag("x", vec![1]), bag("x", vec![2])],
            vec![bag("yy", vec![1]), bag("yy", vec![2])],
            vec![bag("zzz", vec![1]), bag("zzz", vec![2])],
        ],
        "fan_out Vec<struct> aggregate mis-decoded"
    );
    println!("OK flatten_bags {groups:?}");
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

// --- attribute injection: service_account (concat) + node_selector (arg) ---
//
// Both lower to `{{=fromJSON(inputs.parameters[...])}}` on the leaf
// container template (Template.ServiceAccountName / Template.NodeSelector),
// where Argo's per-template SubstituteParams resolves them before
// pod creation. (The boundary-copy footgun applies to
// `boundary_node_selector`, not the per-container form: proven on
// v4.0.5, probe 2026-05-27.) `default` is the SA the e2e namespace
// uses; `amd64` matches the kind node label.
#[container(
    service_account = "defa" + suffix,
    node_selector = { "kubernetes.io/arch" = arch },
)]
pub fn scheduled(suffix: String, arch: String) {
    println!("scheduled sa+ns inj: suffix={suffix} arch={arch}");
}

// --- artifact ports (save -> load, ordered by a threaded dep) --------------

#[container]
pub fn seed_note() -> String {
    cargo_athena::save_artifact_str!("e2e-note", "payload-42");
    "seeded".to_string()
}

#[container]
pub fn read_note(_after: String) {
    let v = cargo_athena::load_artifact_str!("e2e-note");
    println!("read e2e-note = {v}");
}

// --- non-trivial parameter round-trip --------------------------------------
//
// Two complementary paths exercised:
//
//   1. `make_blob` -> `verify_blob`: cross-task transfer. Producer
//      writes the blob to `outputs.parameters.return`; consumer reads
//      it as positional argv after Argo substitutes
//      `{{tasks.X.outputs.parameters.return}}`. Capped at 16 KB - the
//      output side has no offload feature; the wait container ships
//      values to the controller via a 2 MB gRPC channel, well above
//      this but below the practical safe ceiling.
//
//   2. `verify_input_blob`: workflow-input -> consumer arg. The e2e
//      script submits with a 200 KB literal via `--parameter-file`
//      (downsized on Argo 3.6, which lacks args-offload). Past 128 KB
//      Argo's controller offloads the consumer's whole `c.Args[]` to
//      a per-pod ConfigMap and replaces any single arg > 128 KB with
//      a `@/tmp/argo_arg_<i>.txt` sentinel; athena's runtime
//      (`entrypoint_impl` -> `deref_offloaded_arg`) reads that file
//      back transparently.

const BLOB_BYTES: usize = 16 * 1024;

#[container]
pub fn make_blob() -> String {
    "x".repeat(BLOB_BYTES)
}

#[container]
pub fn verify_blob(blob: String) {
    assert_eq!(blob.len(), BLOB_BYTES, "blob round-trip size drift");
    assert!(blob.chars().all(|c| c == 'x'), "blob content drift");
    println!("OK verify_blob {} bytes", blob.len());
}

/// Asserts content (all `'x'`) but not a fixed size: the e2e script
/// downsizes on Argo 3.6, where the offload feature is absent.
#[container]
pub fn verify_input_blob(blob: String) {
    assert!(!blob.is_empty(), "input_blob arrived empty");
    assert!(blob.chars().all(|c| c == 'x'), "input_blob content drift");
    println!("OK verify_input_blob {} bytes", blob.len());
}

// --- per-task builders + the exit handler ----------------------------------

#[container]
pub fn risky() -> String {
    "ok".to_string()
}

#[container]
pub fn finalize(r: Result<String, cargo_athena::ArgoError>) {
    println!("finalize:{}", r.expect("risky must succeed"));
}

#[container]
pub fn risky_fail() -> String {
    panic!("deliberate failure (continue_on e2e)")
}

#[container]
pub fn expect_err(r: Result<String, cargo_athena::ArgoError>) {
    let e = r.expect_err("continue_on of a failed task must be Err");
    println!("OK expect_err status={} exit={:?}", e.status, e.exit_code);
}

/// Fails on `"b"` — proves the whole fan is Err on ONE bad element.
#[container]
pub fn flaky(x: String) -> String {
    if x == "b" {
        panic!("deliberate element failure: {x}");
    }
    x.to_uppercase()
}

#[container]
pub fn always_fail(x: String) -> String {
    panic!("deliberate element failure: {x}")
}

#[container]
pub fn expect_fan_err(tag: String, r: Result<Vec<String>, cargo_athena::ArgoError>) {
    let e = r.expect_err("fan_out with failures must be Err");
    println!("OK expect_fan_err {tag} status={}", e.status);
}

#[container]
pub fn expect_fan_ok(r: Result<Vec<String>, cargo_athena::ArgoError>) {
    assert_eq!(r.expect("all-success fan must be Ok"), vec!["A", "B", "C"]);
    println!("OK expect_fan_ok");
}

/// `on_exit_if_root` handler (whole-workflow) AND a per-task
/// `.on_exit(cleanup)` target — both should run.
#[container]
pub fn cleanup() {
    println!("cleanup ran");
}

/// The whole feature matrix in one DAG. `node_selector_if_root` lands
/// on every pod in the run (via WorkflowSpec.NodeSelector — the only
/// nodeSelector tier that reaches nested sub-workflows' pods);
/// `on_exit_if_root` fires `cleanup` once at the end (only because
/// *this* is the submitted root).
#[workflow(
    node_selector_if_root = { "kubernetes.io/arch" = "amd64" },
    on_exit_if_root = cleanup,
)]
pub fn pipeline(input_blob: String) {
    let a = produce();
    let b = transform(a); // container -> container param dep
    consume(b);

    let s = finalize_wf(); // nested workflow returns a value
    audit(s); // workflow -> container dep

    note(echo("nested".to_string())); // nested call: echo then note(dep)

    // fan_out element-type matrix — each consumer asserts the exact
    // decoded value, so any mis-encoding fails the workflow:
    let items = make_list();
    let caps = items.fan_out(|x| upper(x)); // Vec<String>  (scalar row)
    summarize(caps);

    let empty = make_empty();
    let none = empty.fan_out(|x| upper(x)); // empty source -> [] aggregate
    expect_empty(none);

    // continue_on fan_out: Result<Vec<T>, ArgoError>, all-or-nothing.
    let l1 = make_list();
    let part = l1.fan_out(|x| flaky(x)).continue_on(failed);
    expect_fan_err("partial".to_string(), part); // ONE bad element => Err

    let l2 = make_list();
    let nothing = l2.fan_out(|x| always_fail(x)).continue_on(failed);
    expect_fan_err("allfail".to_string(), nothing);

    let l3 = make_list();
    let full = l3.fan_out(|x| upper(x)).continue_on(failed);
    expect_fan_ok(full); // no failures => Ok(full vec)

    let bad = risky_fail().continue_on(failed);
    expect_err(bad); // continue_on task: Err{status, exit_code}

    let raw = make_list();
    let bags = raw.fan_out(|x| pack(x)); // Vec<Bag>  (struct row)
    collect_bags(bags);

    let w1 = make_words();
    let lens = w1.fan_out(|x| wlen(x)); // Vec<i64>  (scalar row)
    sum_lens(lens);

    let w2 = make_words();
    let evens = w2.fan_out(|x| is_even(x)); // Vec<bool>  (scalar row)
    count_true(evens);

    let w3 = make_words();
    let groups = w3.fan_out(|x| pair(x)); // Vec<Vec<Bag>>  (array row)
    flatten_bags(groups);

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
    scheduled("ult".to_string(), "amd64".to_string()); // sa+ns injection

    let k = seed_note(); // save_artifact!
    read_note(k); // load_artifact!, ordered after seed_note via `k`

    let r = risky().continue_on(failed, error);
    finalize(r).on_exit(cleanup); // per-task exit hook

    // Multi-KB parameter round-trip: producer writes blob -> Argo
    // captures it as outputs.parameters.return -> consumer receives
    // it as positional argv -> serde decodes back to String.
    let blob = make_blob();
    verify_blob(blob);

    // Workflow-input -> consumer arg. The e2e script submits with a
    // >128 KB literal on Argo 3.7+ to exercise the controller's
    // args-offload + athena's @filename de-reference (a smaller value
    // on 3.6, which lacks the feature).
    verify_input_blob(input_blob);

    // Force-link the steps-mode workflow into this entrypoint's emit
    // closure so its template registers + Argo executes it as a
    // sub-workflow under the mega-pipeline (cross-template steps:
    // semantics asserted live). The e2e script also submits it as a
    // top-level root, separately.
    pipeline_steps();
}

/// `#[workflow(steps)]` live coverage: emits an Argo `steps:` template
/// (one sequential group per statement, cross-step refs as
/// `{{steps.X.outputs.parameters.return}}`). The e2e script submits
/// this against real Argo and asserts `Succeeded` — guards against the
/// macro-only/golden-only steps coverage regressing on a real cluster.
/// Reuses existing containers; nothing new needed in-pod.
///
/// Pipeline declares `node_selector_if_root` (WorkflowSpec.NodeSelector,
/// root-only) — that's Argo's only nodeSelector tier that lands on
/// every pod in the run regardless of nesting. So this sub-workflow's
/// pods inherit it from wfSpec without any re-declaration here.
#[workflow(steps)]
pub fn pipeline_steps() {
    let s = stamp();
    let r = echo(s);
    audit(r);
}
