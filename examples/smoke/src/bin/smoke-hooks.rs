//! Rooted at `pipeline_hooks`. Its golden pins the per-task builder
//! emit: `continueOn`, the `exit` hook, and an expression hook — plus
//! that hook templates are force-linked + emitted via the wormhole.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_hooks);
}
