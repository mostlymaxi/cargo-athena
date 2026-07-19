// `boundary_tolerations` entries get the same operator/effect
// closed-set checks as `tolerations` (all literal by design).
#[cargo_athena::workflow(boundary_tolerations = [
    { key = "gpu", operator = "Sometimes", effect = "NoSchedule" },
])]
fn pipeline() {}

fn main() {}
