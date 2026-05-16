// A macro statement in a #[workflow] is unmodeled — must fail loudly
// rather than vanish from the emitted DAG.
#[cargo_athena::workflow]
fn wf() {
    println!("hello");
}

fn main() {}
