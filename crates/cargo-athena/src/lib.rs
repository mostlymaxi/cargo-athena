//! `cargo-athena` — compile regular Rust into Argo Workflow YAML.
//!
//! This facade is the only crate users depend on. It re-exports the
//! runtime ([`cargo_athena_core`]) and the proc macros
//! ([`cargo_athena_macros`]) behind one stable `::cargo_athena` path, which is
//! also the path the generated code targets.
//!
//! ```ignore
//! use cargo_athena::{workflow, container};   // `host!` is used path-qualified
//!
//! #[workflow]
//! fn run_foo() {
//!     let a = some_other_workflow("asdf".to_string());
//!     run_a_container(a);
//! }
//!
//! #[container(image = "ghcr.io/acme/app:latest")]
//! fn run_a_container(a: String) {
//!     let cfg = cargo_athena::host!("/etc/myapp");  // -> hostPath volume; compile error outside #[container]/#[fragment]
//!     println!("{cfg} {a}");
//! }
//!
//! // entrypoint is a *type*; referencing it force-links the closure.
//! fn main() { cargo_athena::entrypoint::<run_foo>(); }
//! ```

// Runtime: modes, registration, BuildCtx, YAML emit, `host!`, re-exported
// `api` / `inventory` / `serde_json` / `serde_norway`.
pub use cargo_athena_core::*;

// `#[macro_export]` macros live at the dependency's crate root; re-export
// explicitly so `cargo_athena::host!` resolves for users. `__cargo_athena_host`
// is the private real macro the attribute macros rewrite visible `host!`s
// into — it must be reachable at `::cargo_athena::__cargo_athena_host`.
#[doc(hidden)]
pub use cargo_athena_core::{
    __cargo_athena_host, __cargo_athena_load_artifact, __cargo_athena_load_artifact_str,
    __cargo_athena_save_artifact, __cargo_athena_save_artifact_str,
};
pub use cargo_athena_core::{
    host, load_artifact, load_artifact_str, save_artifact, save_artifact_str,
};

// Attribute macros.
pub use cargo_athena_macros::{container, fragment, workflow};
