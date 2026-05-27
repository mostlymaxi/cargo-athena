//! Rooted at `pipeline_fanout`. Golden pins the `.fan_out` lowering:
//! a `withParam` task over the list + `{{item}}` arg + the aggregated
//! `Vec<U>` consumed downstream.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_fanout);
}
