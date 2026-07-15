// Surplus arguments to `secret!` hit the public compile_error gate
// instead of being silently dropped by the rewriter.
#[cargo_athena::container]
fn c() {
    let _ = cargo_athena::secret!("name", "key", "extra");
}

fn main() {}
