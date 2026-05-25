// Ghost-checked: a producer returning `Artifact<String>` cannot feed a
// consumer expecting plain `String`. The ghost's `__athena_sig` mirrors
// each real signature, so the type-mismatch is caught by Rust's own
// type system, not a bespoke macro check.

#[cargo_athena::container]
fn make_str_artifact() -> cargo_athena::Artifact<String> {
    cargo_athena::Artifact::new("hi".to_string())
}

#[cargo_athena::container]
fn use_str(s: String) {
    let _ = s;
}

#[cargo_athena::workflow]
fn pipeline() {
    let a = make_str_artifact();
    use_str(a);
}

fn main() {}
