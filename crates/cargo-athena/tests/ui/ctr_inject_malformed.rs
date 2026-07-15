// A malformed `#[inject(..)]` body (anything but a single string
// literal) is a hard error — previously it was silently ignored and
// the arg quietly demoted to a normal parameter.
#[cargo_athena::container]
fn c(#[inject(1 + 2)] attempt: i64, payload: String) -> String {
    format!("{attempt}:{payload}")
}

fn main() {}
