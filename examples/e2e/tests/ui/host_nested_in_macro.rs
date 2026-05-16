// Even INSIDE a #[container], `host!` nested in another macro's tokens
// must not compile: the AST scanner can't see into `println!`'s tokens,
// so it couldn't be collected — failing loud beats an unmounted path.
#[cargo_athena::container]
fn c() {
    println!("{}", cargo_athena::host!("/nope"));
}

fn main() {}
