// A constant arg is fixed at emit: a non-`Copy` static (here a
// `LazyLock`) is rejected. Use a `const` item.
use std::sync::LazyLock;

static IMG: LazyLock<String> = LazyLock::new(|| "img".to_string());

#[cargo_athena::container]
fn take(s: String) {
    println!("{s}");
}

#[cargo_athena::workflow]
fn wf() {
    take(IMG.clone());
}

fn main() {}
