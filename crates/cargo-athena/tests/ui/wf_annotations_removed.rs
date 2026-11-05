// `#[workflow(annotations)]` was removed: Argo never applies a dag/steps
// template's metadata to any pod.
#[cargo_athena::workflow(annotations = { "k" = "v" })]
fn f() {}

fn main() {}
