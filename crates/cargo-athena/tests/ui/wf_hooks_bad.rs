// Each `.hooks(...)` entry must be `"argo-expression" = template`, not a
// bare path.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::container]
fn notify() {}

#[cargo_athena::workflow]
fn wf() {
    step("a".to_string()).hooks(notify);
}

fn main() {}
