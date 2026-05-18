// `pod_gc_if_root(...)` requires `strategy = "..."`; omitting it is a
// targeted compile error.
#[cargo_athena::workflow(pod_gc_if_root())]
fn f() {}

fn main() {}
