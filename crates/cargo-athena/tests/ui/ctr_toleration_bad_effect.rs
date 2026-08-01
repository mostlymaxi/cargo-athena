// A literal K8s `Toleration.effect` is checked against the closed set
// (NoSchedule|PreferNoSchedule|NoExecute, or empty = match all). An
// injected / `{{…}}`-substituted effect is left to k8s at admission.
#[cargo_athena::container(tolerations = [
    { key = "gpu", operator = "Exists", effect = "Sometimes" },
])]
fn f() {}

fn main() {}
