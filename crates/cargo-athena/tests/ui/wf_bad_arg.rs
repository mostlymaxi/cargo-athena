// A regular variable/const as a #[workflow] call arg is unsupported:
// only literals, workflow inputs, and prior `let` bindings are allowed.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::workflow]
fn wf() {
    step(some_global);
}

fn main() {}
