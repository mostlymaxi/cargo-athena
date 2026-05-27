//! Rooted at `pipeline_ttl`. Golden pins WorkflowSpec-scoped
//! `ttlStrategy`/`podGC` from `#[workflow(ttl(..), pod_gc(..))]`.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_ttl);
}
