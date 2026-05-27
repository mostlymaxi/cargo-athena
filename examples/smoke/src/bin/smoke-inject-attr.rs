//! Rooted at `pipeline_inject_attr`. Golden pins:
//! - `inputs.parameters` declares only `payload` (the inject args are
//!   filled by Argo, not by the caller, so they don't appear here).
//! - `container.args[]` has 3 positional slots in fn-declaration order:
//!   `{{inputs.parameters.payload}}`, `{{retries}}`,
//!   `"{{pod.name}}"`.
//! - The workflow body's `smart_retry("hello".to_string())` call only
//!   passes the `payload` arg.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_inject_attr);
}
