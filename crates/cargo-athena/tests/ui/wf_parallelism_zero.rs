// `parallelism = 0` — Argo's CRD enforces Minimum=1; must be > 0.
#[cargo_athena::workflow(parallelism = 0)]
fn pipeline() {}

fn main() {}
