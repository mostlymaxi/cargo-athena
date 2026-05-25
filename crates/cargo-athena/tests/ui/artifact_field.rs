// Ghost-checked: `a.field` access requires `a` be a struct with that
// field. `Artifact<T>` has no public fields by design, so field access
// inside a workflow body (rewritten to the ghost) fails as a normal
// Rust field-access error. No bespoke macro check involved.

#[cargo_athena::container]
fn make_str() -> cargo_athena::Artifact<String> {
    cargo_athena::Artifact::new("hi".to_string())
}

#[cargo_athena::container]
fn use_str(s: String) {
    let _ = s;
}

#[cargo_athena::workflow]
fn pipeline() {
    let a = make_str();
    use_str(a.inner);
}

fn main() {}
