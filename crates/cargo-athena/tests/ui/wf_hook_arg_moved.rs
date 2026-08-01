// Faithful move semantics extend to hook args: feeding one binding to
// a task AND its hook is a fan-out (Argo copies the output param into
// each consumer), so it needs an explicit `.clone()` — same rule as
// task-arg fan-out.
#[cargo_athena::container]
fn fetch() -> String {
    "v".to_string()
}

#[cargo_athena::container]
fn publish(x: String) {
    println!("{x}");
}

#[cargo_athena::container]
fn notify(x: String) {
    println!("{x}");
}

#[cargo_athena::workflow]
fn wf() {
    let r = fetch();
    publish(r).on_failure(notify(r));
}

fn main() {}
