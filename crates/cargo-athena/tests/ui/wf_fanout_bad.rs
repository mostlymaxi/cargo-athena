// A `fan_out` closure body must be a template call (not a bare value /
// arbitrary expression).
#[cargo_athena::container]
fn make_list() -> Vec<String> {
    vec![]
}

#[cargo_athena::workflow]
fn wf() {
    let a = make_list();
    let _b = a.fan_out(|x| x);
}

fn main() {}
