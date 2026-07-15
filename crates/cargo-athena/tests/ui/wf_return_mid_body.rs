// A mid-body `return` is rejected: a #[workflow] lowers to a DAG, so
// statements after the `return` would still run as tasks in Argo, and
// a later `return` would overwrite the output task (last-wins — the
// opposite of Rust).
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::workflow]
fn wf() -> String {
    return step("a".to_string());
    step("b".to_string());
}

fn main() {}
