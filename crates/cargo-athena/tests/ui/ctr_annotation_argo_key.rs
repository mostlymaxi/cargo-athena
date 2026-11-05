// A key Argo sets on every pod would overwrite the controller's own annotation.
#[cargo_athena::container(annotations = { "workflows.argoproj.io/node-id" = "x" })]
fn f() {}

fn main() {}
