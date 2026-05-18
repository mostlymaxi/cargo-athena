// `retry(...)` requires `limit = N` (or `limit = unlimited`); omitting
// it is a targeted compile error.
#[cargo_athena::container(retry(policy = "OnError"))]
fn f() {}

fn main() {}
