// Hook target calls are ghost-type-checked like task calls: wrong
// arity is a compile error, not a nameless parameter in the YAML.
// Covers both directions — an excess arg on a call target and a bare
// path target whose template declares inputs.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::container]
fn notify(msg: String) {
    println!("{msg}");
}

#[cargo_athena::workflow]
fn wf() {
    step("a".to_string()).on_success(notify("m".to_string(), "extra".to_string()));
    step("b".to_string()).on_exit(notify);
}

fn main() {}
