// Ghost-checked: `.fan_out` requires `AthenaList<_>`, which `Artifact<T>`
// does not implement. The error is a normal Rust trait-bound diagnostic
// from the workflow's hidden ghost, no bespoke macro check involved.
// Proves the "ghost-first" principle: `Artifact<T>` blocks an invalid
// caller-side operation through Rust's own type rules.

#[cargo_athena::container]
fn make_list() -> cargo_athena::Artifact<Vec<String>> {
    cargo_athena::Artifact::new(vec!["a".to_string()])
}

#[cargo_athena::container]
fn consume(s: String) {
    let _ = s;
}

#[cargo_athena::workflow]
fn pipeline() {
    let a = make_list();
    let _ = a.fan_out(|x| consume(x));
}

fn main() {}
