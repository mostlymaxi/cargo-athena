//! Rooted at `pipeline_nested`. Golden pins nested-call lowering: a
//! template call in argument position (recursive, wired as an output
//! ref + dep) and a call hoisted out of an `if` condition.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_nested);
}
