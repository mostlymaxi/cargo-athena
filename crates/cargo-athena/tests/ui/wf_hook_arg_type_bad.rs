// Hook target calls are ghost-type-checked like task calls: an arg
// type mismatch is a compile error.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::container]
fn notify(count: i64) {
    println!("{count}");
}

#[cargo_athena::workflow]
fn wf() {
    step("a".to_string()).on_failure(notify("nope"));
}

fn main() {}
