// A generic fn can't be a template (one unit-struct identity, one
// concrete signature) — a spanned "unsupported" error instead of
// rustc noise inside hidden generated items.
#[cargo_athena::container]
fn f<T: ToString>(x: String) {
    let _ = x;
}

fn main() {}
