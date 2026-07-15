// Same as wf_return_fan_out, but as a value-`if` arm: the arm body is
// its own synthesized sub-workflow, and its `return` bubble has the
// same raw-aggregate problem.
#[cargo_athena::container]
fn make() -> Vec<String> {
    vec![]
}

#[cargo_athena::container]
fn caps(s: String) -> String {
    s
}

#[cargo_athena::workflow]
fn wf(count: i64) -> Vec<String> {
    if count > 3 {
        let xs = make();
        xs.fan_out(|x| caps(x))
    } else {
        make()
    }
}

fn main() {}
