//! Proc macros that compile annotated Rust fns into Argo templates.
//!
//! * `#[container]` — leaf. The fn body stays real code (run in-pod); we
//!   add a run-mode dispatcher and a template builder. `host!`/callees are
//!   collected **statically** (AST union over every branch) so resource
//!   declarations are never missed regardless of control flow.
//! * `#[workflow]` — composition. The body is *analyzed*, not compiled:
//!   straight-line `let x = callee(args);` becomes a DAG; data deps become
//!   Argo parameter wiring. (Hybrid seam: this is the static analyzer that
//!   later lowers into the promise-graph for richer control flow.)
//! * `#[fragment]` — a plain helper fn that carries `host!` decls; it
//!   propagates resources up the static call graph (cross-item case).
//!
//! ## Module map
//!
//! Proc-macro `#[proc_macro_attribute]` entry points must live in the
//! crate root, so this file is intentionally thin: each entry point is a
//! one-line delegate to its module's `expand` fn.
//!
//! - `utils` — name munging, fn-arg shape, static body scan, decl-macro
//!   gating, YAML-safe-name guard, AST navigation.
//! - `attrs` — `#[container]`/`#[workflow]` attribute structs (deluxe-
//!   parsed), `inject_lower`, the duration parsers and token lowerings.
//! - `ghost` — the never-run type-check ghost for `#[workflow]` bodies.
//! - `container` / `workflow` / `fragment` — one per proc-macro,
//!   each owning the full expansion.
//! - `analyze` — workflow body analyzer (`Arg`/`Node` types,
//!   `analyze_workflow`, the per-call lowering).
//! - `conditional` — `if`/`else` → `when`-gated synthetic wrappers.
//! - `node_tokens` — per-task `quote!` builder (the only producer of
//!   the Argo `templateRef` + parameter + dependency wiring).

use proc_macro::TokenStream;

mod analyze;
mod attrs;
mod conditional;
mod container;
mod fragment;
mod ghost;
mod node_tokens;
mod utils;
mod workflow;

#[doc = include_str!("../CONTAINER.md")]
#[proc_macro_attribute]
pub fn container(attr: TokenStream, item: TokenStream) -> TokenStream {
    container::expand(attr, item)
}

#[doc = include_str!("../WORKFLOW.md")]
#[proc_macro_attribute]
pub fn workflow(attr: TokenStream, item: TokenStream) -> TokenStream {
    workflow::expand(attr, item)
}

/// A plain helper function (not a template) that carries pod-resource
/// declarations (`host!`, artifact ports) across function boundaries
/// into every `#[container]` that transitively calls it. It runs as
/// ordinary Rust inside the caller's pod and cannot be called from a
/// `#[workflow]`. See the **`#[fragment]`** section of the
/// [`macro@container`] docs (`CONTAINER.md`) for the full model.
#[proc_macro_attribute]
pub fn fragment(attr: TokenStream, item: TokenStream) -> TokenStream {
    fragment::expand(attr, item)
}
