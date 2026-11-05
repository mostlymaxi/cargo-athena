// A call whose name starts uppercase is read as a constructor. A plain
// fn named like one fails the ghost's pattern check.
#[allow(non_snake_case)]
fn Now() -> u32 {
    3
}

#[cargo_athena::container]
fn take_n(count: u32) {
    println!("{count}");
}

#[cargo_athena::workflow]
fn wf() {
    take_n(Now());
}

fn main() {}
