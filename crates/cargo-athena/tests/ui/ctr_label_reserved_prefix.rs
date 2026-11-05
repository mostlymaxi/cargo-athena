// Argo copies template labels over its own pod labels; overriding one
// would break the controller's pod tracking.
#[cargo_athena::container(labels = { "workflows.argoproj.io/workflow" = "x" })]
fn f() {}

fn main() {}
