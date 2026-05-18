// `ttl_if_root(...)` needs at least one of after_completion/
// after_success/after_failure; an empty `ttl_if_root()` is a
// targeted compile error.
#[cargo_athena::workflow(ttl_if_root())]
fn f() {}

fn main() {}
