//! Rooted at `pipeline_consts`. Golden pins constant args: `const`
//! items, struct / tuple / enum literals, constant `let` + `.field`, and
//! a `const` in an `if` condition.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_consts);
}
