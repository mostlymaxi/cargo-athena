// `ttl(...)` needs at least one of after_completion/after_success/
// after_failure; an empty `ttl()` is a targeted compile error.
#[cargo_athena::workflow(ttl())]
fn f() {}

fn main() {}
