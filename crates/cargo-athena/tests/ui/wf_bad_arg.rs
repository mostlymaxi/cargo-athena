// An unknown name in a #[workflow] call arg is rustc's own error: a
// name that isn't an input or a binding is read as a constant.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::workflow]
fn wf() {
    step(some_global);
}

fn main() {}
