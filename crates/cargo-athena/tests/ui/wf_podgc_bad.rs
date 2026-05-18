// `pod_gc_if_root(strategy = ...)` must be one of OnPodCompletion|
// OnPodSuccess|OnWorkflowCompletion|OnWorkflowSuccess; anything else
// is a targeted compile error.
#[cargo_athena::workflow(pod_gc_if_root(strategy = "Nope"))]
fn f() {}

fn main() {}
