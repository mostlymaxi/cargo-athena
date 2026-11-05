// A literal label value must be a valid k8s label value.
#[cargo_athena::container(labels = { "team" = "has space" })]
fn f() {}

fn main() {}
