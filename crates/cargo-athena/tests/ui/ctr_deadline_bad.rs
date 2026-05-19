// `pod_running_timeout = "later"` is not a parseable humantime
// duration — a targeted compile error (on a valid `#[container]`).
#[cargo_athena::container(pod_running_timeout = "later")]
fn f() {}

fn main() {}
