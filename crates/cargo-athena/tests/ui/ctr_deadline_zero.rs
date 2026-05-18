// `active_deadline = 0` — must be a positive number of seconds;
// zero is a targeted compile error.
#[cargo_athena::container(active_deadline = 0)]
fn f() {}

fn main() {}
