// A method-call statement in a #[workflow] is not a template call —
// unmodeled, so it must be a compile error.
#[cargo_athena::workflow]
fn wf() {
    "x".to_string().len();
}

fn main() {}
