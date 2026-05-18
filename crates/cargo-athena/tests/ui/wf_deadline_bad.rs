// `active_deadline = "later"` is not a parseable humantime duration —
// a targeted compile error.
#[cargo_athena::workflow(active_deadline = "later")]
fn f() {}

fn main() {}
