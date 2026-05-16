//! Same binary machinery, rooted at a workflow that lives in *another
//! module* and pulls in the crate-root `pipeline`. If module resolution
//! or the type-wormhole closure regressed, this emit would be missing
//! templates and its golden would fail.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_e2e::another::pipeline_another>();
}
