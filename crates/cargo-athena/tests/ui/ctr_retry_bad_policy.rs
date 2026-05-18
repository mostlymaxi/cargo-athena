// `retry(policy = ...)` must be one of Always|OnFailure|OnError|
// OnTransientError; anything else is a targeted compile error.
#[cargo_athena::container(retry(limit = 1, policy = "Nope"))]
fn f() {}

fn main() {}
