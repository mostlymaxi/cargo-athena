// v1 supports named struct fields (`a.b.c`) only — tuple-field access
// (`a.0`) and index access (`a[i]`) are deferred, with a targeted error.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::container]
fn sink(v: String) {
    println!("{v}");
}

#[cargo_athena::workflow]
fn wf() {
    let a = step("x".to_string());
    sink(a.0);
}

fn main() {}
