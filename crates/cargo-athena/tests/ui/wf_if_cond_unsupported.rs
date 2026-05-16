// `if` conditions are a closed grammar: comparisons / && / || / ! over a
// binding, input, `a.field`, or literal. A method call (here `.len()`)
// is a targeted compile_error — never a mistranslated `when`.
#[cargo_athena::container]
fn val() -> String {
    String::new()
}

#[cargo_athena::container]
fn act() {}

#[cargo_athena::workflow]
fn wf() {
    let s = val();
    if s.len() > 0 {
        act();
    }
}

fn main() {}
