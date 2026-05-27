//! Rooted at `pipeline_parallelism`. Golden pins both
//! `Template.parallelism` (template-level) and
//! `WorkflowSpec.parallelism` (root-only, via the `_if_root` attr).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_parallelism);
}
