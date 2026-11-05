// A constant of the wrong type: the ghost and the emit slot both report
// the mismatch.
const NAME: &str = "x";

#[cargo_athena::container]
fn take_n(count: u32) {
    println!("{count}");
}

#[cargo_athena::workflow]
fn wf() {
    take_n(NAME);
}

fn main() {}
