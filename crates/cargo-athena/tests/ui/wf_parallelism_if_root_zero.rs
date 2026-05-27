// `parallelism_if_root = 0` — Argo's CRD enforces Minimum=1; must
// be > 0 (a `0` here would also deadlock the run at runtime).
#[cargo_athena::workflow(parallelism_if_root = 0)]
fn pipeline() {}

fn main() {}
