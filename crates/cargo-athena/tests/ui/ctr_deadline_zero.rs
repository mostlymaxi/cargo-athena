// `pod_running_timeout = 0` — must be a positive number of seconds;
// zero is a targeted compile error.
#[cargo_athena::container(pod_running_timeout = 0)]
fn f() {}

fn main() {}
