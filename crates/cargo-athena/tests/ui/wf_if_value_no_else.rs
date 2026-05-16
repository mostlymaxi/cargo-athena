// An `if` used as a value must have an `else` (both branches produce the
// value). Spanned, fail-loud — never a half-lowered wrapper.
#[cargo_athena::container]
fn flag() -> bool {
    true
}

#[cargo_athena::container]
fn act() {}

#[cargo_athena::workflow]
fn wf() {
    let f = flag();
    let _b = if f { act() };
}

fn main() {}
