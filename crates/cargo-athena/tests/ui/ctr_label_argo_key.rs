// A key Argo sets on every pod would overwrite the controller's own label.
#[cargo_athena::container(labels = { "workflows.argoproj.io/workflow" = "x" })]
fn f() {}

fn main() {}
