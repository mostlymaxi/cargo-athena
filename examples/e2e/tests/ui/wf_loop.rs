// Loops aren't lowered yet: a `for` in a #[workflow] must be a clear
// compile error, not a silently dropped task.
#[cargo_athena::workflow]
fn wf() {
    for _ in 0..3 {}
}

fn main() {}
