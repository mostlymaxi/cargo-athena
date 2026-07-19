// K8s `Toleration.operator` is a closed set (Equal|Exists); anything
// else would only fail at k8s admission — reject at compile time,
// spanned at the literal.
#[cargo_athena::container(tolerations = [
    { key = "gpu", operator = "Banana", effect = "NoSchedule" },
])]
fn f() {}

fn main() {}
