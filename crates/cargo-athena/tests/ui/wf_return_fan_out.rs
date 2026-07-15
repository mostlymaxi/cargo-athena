// A `fan_out` binding cannot be the workflow's return value: the
// `outputs.parameters.return` bubble copies the raw Argo aggregate,
// whose elements are still individually JSON-encoded — the parent
// would read a double-encoded array.
#[cargo_athena::container]
fn make() -> Vec<String> {
    vec![]
}

#[cargo_athena::container]
fn caps(s: String) -> String {
    s
}

#[cargo_athena::workflow]
fn wf() -> Vec<String> {
    let xs = make();
    let ys = xs.fan_out(|x| caps(x));
    ys
}

fn main() {}
