// `while` isn't lowered in a #[workflow] body. Should produce a
// targeted error pointing at `.fan_out` / sub-workflow.
#[cargo_athena::workflow]
fn wf() {
    while false {}
}

fn main() {}
