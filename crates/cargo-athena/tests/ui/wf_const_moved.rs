// A constant `let` keeps Rust move semantics: a non-`Copy` value used
// twice needs `.clone()`.
#[cargo_athena::container]
fn a(s: String) {
    println!("{s}");
}

#[cargo_athena::container]
fn b(s: String) {
    println!("{s}");
}

#[cargo_athena::workflow]
fn wf() {
    let s = "x".to_string();
    a(s);
    b(s);
}

fn main() {}
