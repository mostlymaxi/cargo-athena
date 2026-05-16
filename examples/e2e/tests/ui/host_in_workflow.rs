// `host!` in a #[workflow] must not compile: a workflow is a DAG, not a
// pod. The #[workflow] macro detects it and emits a targeted error.
#[cargo_athena::workflow]
fn wf() {
    let _ = cargo_athena::host!("/nope");
}

fn main() {}
