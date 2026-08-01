// A `name = "..."` override becomes the emitted WorkflowTemplate's
// `metadata.name` — k8s requires a DNS-1123 subdomain, so uppercase /
// `_` fail at compile time instead of at admission.
#[cargo_athena::container(name = "My_Container")]
fn f() {}

fn main() {}
