// A #[workflow] that declares a return type but whose body doesn't end in
// a tail template call (or `return` of a binding) must fail: there's no
// task whose `result` could become the workflow's output.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::workflow]
fn wf() -> String {
    step("a".to_string()); // trailing `;` => not the returned value
}

fn main() {}
