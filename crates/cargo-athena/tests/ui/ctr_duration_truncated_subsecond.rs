// "1h500ms" used to silently truncate to 3600s; a fractional-second
// component is now always a hard error, never a silent rounding.
#[cargo_athena::container(timeout = "1h500ms")]
fn f() {}

fn main() {}
