//! Minimal example on the new (type-identity) API.
//!
//!   cargo run -p cargo-athena-example-basic
//!     -> emits a multi-doc WorkflowTemplate stream rooted at `run_foo`
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-basic-run-a-container \
//!     cargo run -p cargo-athena-example-basic -- '"hi"'
//!     -> runs that container's real body in-process (`-- '"hi"'`
//!        passes the JSON-encoded `a: String` arg as positional argv)

// `host!` is context-restricted; always called path-qualified.
use cargo_athena::{container, fragment, workflow};

#[workflow]
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);
}

#[workflow]
fn some_other_workflow(b: String) -> String {
    let p = prepare(b);
    finalize(p.clone()); // `p` fans out (finalize + the return) -> explicit
    p // `.clone()` mirrors Argo copying the param to each consumer
}

// Opt into Argo `steps:` — each statement is a sequential step group.
#[workflow(steps)]
fn seq_foo() {
    let p = prepare("seed".to_string());
    finalize(p);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");
    load_extra(); // cross-item: pulls /var/lib/extra onto this template
    println!("config dir: {cfg}");
    println!("this is regular code, got: {a}");
}

#[container]
fn prepare(b: String) -> String {
    format!("prepared:{b}")
}

#[container]
fn finalize(p: String) {
    println!("final: {p}");
}

#[fragment]
fn load_extra() {
    let _extra = cargo_athena::host!("/var/lib/extra");
}

fn main() {
    cargo_athena::entrypoint!(run_foo);
}

// Emit-semantics coverage lives in `crates/cargo-athena/tests/smoke.rs`
// (per-template WorkflowTemplate, crate namespacing, cross-item host!
// closure, templateRef, runnable Workflow, `#[workflow(steps)]`,
// `#[workflow]` return values). This stays a pure, minimal example.
