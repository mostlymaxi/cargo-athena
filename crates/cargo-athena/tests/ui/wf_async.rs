// `#[workflow]` bodies are statically analyzed, not executed —
// `async fn` is meaningless here and is a targeted compile error.
// (Only `#[container]` bodies actually run; they may be `async fn`
// with the cargo-athena `async` feature.)
#[cargo_athena::workflow]
async fn pipeline() {}

fn main() {}
