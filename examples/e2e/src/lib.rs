//! Broad-coverage e2e fixture (library form, so it can be imported by
//! other crates). Exercises, in one place:
//!
//! * root `#[workflow]` with a multi-dependency DAG task,
//! * nested `#[workflow]` (workflow-calls-workflow) with an input-param ref,
//! * `#[container]` with explicit `image`/`bin` and with defaults,
//! * mixed arg kinds: string/int literals, `.to_string()`, input refs,
//!   prior-task refs,
//! * `host!` declared across BOTH `if/else` and `match` arms (static union),
//! * transitive `#[fragment]` resource closure (`frag_a` -> `frag_b`),
//! * an intra-crate cross-*module* workflow (`another::pipeline_another`)
//!   that composes a local template with the crate-root `pipeline`.
//!
//! Every `#[workflow]`/`#[container]` is a `pub` type so downstream crates
//! (see `examples/e2e-consumer`) can import and compose it. Keep this file
//! stable; refresh goldens with `UPDATE_EXPECT=1`.

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
pub fn ingest(source: String) {
    let raw = fetch(source); // `source` -> {{inputs.parameters.source}}
    let clean = transform(raw, 2); // raw -> task ref; 2 -> literal
    publish(clean); // clean -> task ref
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

#[container(image = "ghcr.io/acme/combine:latest")]
pub fn combine(x: String, y: String) -> String {
    format!("{x}+{y}")
}

#[container]
pub fn publish(report: String) {
    println!("publishing {report}");
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

// --- intra-crate cross-module composition ----------------------------------

pub mod another {
    //! Proves a workflow in another *module* composes the crate-root
    //! `pipeline` exactly like any other template (same wormhole path).
    use cargo_athena::{container, workflow};

    #[container]
    pub fn local_step(tag: String) -> String {
        format!("local:{tag}")
    }

    #[container]
    pub fn sink(v: String) {
        println!("sink {v}");
    }

    #[workflow]
    pub fn pipeline_another() {
        let s = local_step("m".to_string());
        crate::pipeline(); // cross-module workflow -> workflow
        sink(s); // depends on local_step's output
    }
}
