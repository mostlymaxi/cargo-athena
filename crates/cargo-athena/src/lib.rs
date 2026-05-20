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

// `async fn` `#[container]` support, driven by Tokio. The macro
// detects `async fn` and wraps the body's call in `__async::block_on`,
// so user code stays plain `async fn` with no runtime boilerplate.
// Off by default to keep the lean (sync) library tree small — opt in
// with `cargo-athena = { features = ["tokio"] }`. `tokio` is
// re-exported (`cargo_athena::tokio`) so most async bodies need no
// extra dep; users can bring their own `tokio = { features = […] }`
// for more — cargo unions features across the dep graph. Named for
// the runtime (not the keyword) so other runtimes can land later as
// separate features without colliding.
#[cfg(feature = "tokio")]
pub use tokio;

#[cfg(feature = "tokio")]
#[doc(hidden)]
pub mod __async {
    use std::future::Future;
    /// Drive an `async fn` `#[container]` body to completion on a fresh
    /// single-thread tokio runtime, built per-invocation. Containers
    /// run a single function then exit, so the current-thread runtime
    /// gives the fastest cold-start with no worker threads. `enable_all`
    /// turns on IO + time drivers (covered by the `net`/`time` features).
    pub fn block_on<F: Future>(fut: F) -> F::Output {
        ::tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("athena: build tokio runtime")
            .block_on(fut)
    }
}
