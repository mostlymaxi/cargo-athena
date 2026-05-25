// `mutexes_if_root[].name` injection on a #[workflow] must reference
// one of the workflow's own args (lowered to `workflow.parameters`).
#[cargo_athena::workflow(mutexes_if_root = [{ name = "lock-" + missing }])]
fn pipeline(env: String) {}

fn main() {}
