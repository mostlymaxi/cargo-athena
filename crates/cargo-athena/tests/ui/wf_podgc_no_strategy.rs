// `pod_gc(...)` requires `strategy = "..."`; omitting it is a targeted
// compile error.
#[cargo_athena::workflow(pod_gc())]
fn f() {}

fn main() {}
