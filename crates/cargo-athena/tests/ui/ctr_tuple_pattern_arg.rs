// A destructuring parameter pattern has no Argo parameter name — a
// spanned "unsupported" error instead of rustc noise inside hidden
// generated items.
#[cargo_athena::container]
fn f((a, b): (String, String)) {
    let _ = (a, b);
}

fn main() {}
