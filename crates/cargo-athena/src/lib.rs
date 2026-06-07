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
//! fn main() { cargo_athena::entrypoint!(run_foo); }
//! ```

// Runtime: modes, registration, BuildCtx, YAML emit, `host!`, re-exported
// `api` / `inventory` / `serde_json` / `serde_norway`.
pub use cargo_athena_core::*;

// `#[macro_export]` macros live at the dependency's crate root;
// re-export explicitly so `cargo_athena::host!` etc. resolve for
// users. None of these have a private declarative form — the proc
// macros precompute all derived strings (mount path, artifact file
// path, env var name) at expansion time and emit a direct `rt::*`
// call with the pre-baked literals.
pub use cargo_athena_core::{
    host, load_artifact, load_artifact_str, pvc, save_artifact, save_artifact_str, secret,
    secret_opt,
};

// Attribute macros.
pub use cargo_athena_macros::{container, ephemeral_pvc, external_pvc, fragment, workflow};

/// User-facing entrypoint. Captures the calling binary's identity
/// (`CARGO_PKG_NAME`/`CARGO_PKG_VERSION`/`CARGO_BIN_NAME`) at the user
/// binary's compile time and threads it into [`entrypoint_impl`] so the
/// emitted S3 artifact key (`{crate}/<tag>/{bin}.tar.gz`) matches what
/// `cargo athena publish` uploads. Use as
/// `cargo_athena::entrypoint!(MyRootWorkflow)`.
///
/// It also bakes the build-time version/provenance that names the
/// deployed `WorkflowTemplate`s. `cargo athena build`/`publish` set
/// `ATHENA_VERSION_TAG`/`ATHENA_GIT_*` in the environment before invoking
/// the compile; these are read with `option_env!` (NOT `env!`) so the
/// version identity is *sealed into the binary* yet a plain
/// `cargo install`/`cargo build` (no athena wrapper) still compiles —
/// the vars resolve to `None` and the binary falls back to its baked
/// `CARGO_PKG_VERSION`. `cargo athena build` is the enrichment path,
/// never a requirement for a working binary.
#[macro_export]
macro_rules! entrypoint {
    ($root:ty) => {
        $crate::entrypoint_impl::<$root>(
            ::core::env!("CARGO_PKG_NAME"),
            ::core::env!("CARGO_PKG_VERSION"),
            ::core::env!("CARGO_BIN_NAME"),
            ::core::option_env!("ATHENA_VERSION_TAG"),
            ::core::option_env!("ATHENA_GIT_COMMIT"),
            ::core::option_env!("ATHENA_GIT_DIRTY"),
        )
    };
}

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
