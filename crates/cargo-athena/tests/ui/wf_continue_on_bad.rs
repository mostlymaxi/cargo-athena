// `.continue_on(...)` only accepts the bare idents `failed`/`error`.
#[cargo_athena::container]
fn step(x: String) -> String {
    x
}

#[cargo_athena::workflow]
fn wf() {
    step("a".to_string()).continue_on(maybe);
}

fn main() {}
