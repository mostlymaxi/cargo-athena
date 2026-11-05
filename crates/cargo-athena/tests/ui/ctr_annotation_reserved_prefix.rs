// Argo applies template annotations over its own pod annotations.
#[cargo_athena::container(annotations = { "workflows.argoproj.io/node-id" = "x" })]
fn f() {}

fn main() {}
