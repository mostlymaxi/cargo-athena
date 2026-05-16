//! Rooted at `pipeline_returns`, which consumes a sub-*workflow*'s return
//! value. Its golden pins the `outputs.parameters.result` block that
//! `#[workflow]` now emits and the `{{tasks.r.outputs.result}}` wiring —
//! the proof that workflow→X data deps resolve.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_e2e::pipeline_returns>();
}
