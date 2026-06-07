// `daemon` is a `#[container]`-only attribute. `#[workflow]` has no
// `daemon` field, so `#[workflow(daemon)]` is an unknown-field error by
// construction (no bespoke compile_error! needed).
#[cargo_athena::workflow(daemon)]
fn pipeline() {}

fn main() {}
