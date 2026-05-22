// `secret!`/`secret_opt!` aren't allowed inside a `#[workflow]`:
// workflows are DAGs, not pods. Same rejection as host!/artifact
// macros — declare the secret on the container (or a fragment it
// calls) that actually runs.
#[cargo_athena::workflow]
fn pipeline() {
    let _ = cargo_athena::secret!("api-tokens", "api");
}

fn main() {}
