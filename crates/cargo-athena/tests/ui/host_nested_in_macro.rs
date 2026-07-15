// Even INSIDE a #[container], `host!` nested in another macro's tokens
// must not compile: the AST scanner can't see into `assert!`'s tokens,
// so it couldn't be collected — failing loud beats an unmounted path.
// (The nested expression is type-correct on purpose, so the snapshot
// pins only the athena gate error, not toolchain-dependent rustc notes.)
#[cargo_athena::container]
fn c() {
    assert!(cargo_athena::host!("/nope").is_absolute());
}

fn main() {}
