//! Integration fixture exercised by the kind e2e (`scripts/e2e-test.sh`).
//!
//! Covered end-to-end against a real Argo + MinIO:
//! * `#[workflow]` DAG + a nested `#[workflow]` (templateRef),
//! * container -> container **param data-deps**
//!   (`{{tasks.x.outputs.parameters.return}}`) — proves run-mode
//!   (de)serialize, `ATHENA_PARAM_*` env in, `/athena/result` file out,
//! * a nested `#[workflow]` that **returns a value** consumed by a
//!   downstream container — proves workflow->X data-deps resolve across
//!   the templateRef wormhole (the sub-workflow declares
//!   `outputs.parameters.return`),
//! * default image (busybox) + explicit per-`#[container(image=...)]`,
//! * `host!` hostPath mount + a `#[fragment]` carrying its own `host!`
//!   (cross-item closure lands on the container),
//! * `save_artifact_str!` -> output artifact persisted to MinIO,
//! * binary delivery: cross-compiled musl tarball in MinIO, the
//!   `uname`-resolving bootstrap, scheduled on the worker nodes.

use cargo_athena::{container, fragment, workflow};

#[container]
pub fn produce() -> String {
    // default image (busybox); returns a value other tasks consume.
    "hello".to_string()
}

#[container(image = "busybox:1.36-musl")]
pub fn transform(input: String) -> String {
    // explicit per-container image override path.
    format!("{input}-transformed")
}

#[fragment]
fn extra_mount() {
    // cross-item: this hostPath must land on `consume`'s template.
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
pub fn audit(note: String) {
    // consumes a *workflow's* returned value: workflow -> container dep,
    // i.e. {{tasks.s.outputs.parameters.return}} resolved across templateRef.
    println!("audit:{note}");
}

#[workflow]
pub fn finalize_wf() -> String {
    // Nested workflow that RETURNS a value: its tail call's `return`
    // becomes this workflow-template's own outputs.parameters.return.
    stamp()
}

#[workflow]
pub fn pipeline() {
    let a = produce();
    let b = transform(a); // {{tasks.a.outputs.parameters.return}}
    consume(b); // depends on `b`
    let s = finalize_wf(); // nested workflow via templateRef, returns a value
    audit(s); // workflow -> container: {{tasks.s.outputs.parameters.return}}
}
