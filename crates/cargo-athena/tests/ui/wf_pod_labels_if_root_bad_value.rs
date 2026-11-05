// A literal `pod_labels_if_root` value must be a valid k8s label value.
#[cargo_athena::workflow(pod_labels_if_root = { "team" = "has space" })]
fn f() {}

fn main() {}
