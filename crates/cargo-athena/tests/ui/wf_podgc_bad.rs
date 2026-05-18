// `pod_gc(strategy = ...)` must be one of OnPodCompletion|OnPodSuccess|
// OnWorkflowCompletion|OnWorkflowSuccess; anything else is a targeted
// compile error.
#[cargo_athena::workflow(pod_gc(strategy = "Nope"))]
fn f() {}

fn main() {}
