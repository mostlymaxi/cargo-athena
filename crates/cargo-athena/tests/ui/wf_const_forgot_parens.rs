// `let _r = produce;` (missing `()`) is a constant `let`, not a step:
// the ghost requires it to be `Serialize`, so the step can't silently
// drop.
#[cargo_athena::container]
fn produce() -> u32 {
    1
}

#[cargo_athena::container]
fn other() {}

#[cargo_athena::workflow]
fn wf() {
    let _r = produce;
    other();
}

fn main() {}
