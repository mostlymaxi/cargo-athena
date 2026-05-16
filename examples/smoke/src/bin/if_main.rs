//! Rooted at `pipeline_if`. Golden pins the `if` lowering: synthesized
//! `when`-gated wrapper workflows + a value-`if` whose
//! `outputs.parameters.return` selects the taken branch.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_if>();
}
