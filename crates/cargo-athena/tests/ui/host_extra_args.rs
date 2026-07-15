// Surplus arguments to a decl macro hit the public compile_error gate
// instead of being silently dropped by the rewriter.
#[cargo_athena::container]
fn c() {
    let _ = cargo_athena::host!("/data", "extra");
}

fn main() {}
