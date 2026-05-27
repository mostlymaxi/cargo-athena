//! Rooted at `pipeline_async`. Golden pins the YAML for an async-fn
//! container; the in-pod execution wraps the body in
//! `cargo_athena::__async::block_on` (single-thread tokio runtime).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_async);
}
