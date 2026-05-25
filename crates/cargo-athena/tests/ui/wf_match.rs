// `match` isn't lowered in a #[workflow] body. Should produce a
// targeted error pointing at `if`/`else if`/`else` as the supported
// alternative.
#[cargo_athena::workflow]
fn wf(value: i64) {
    match value {
        0 => {}
        _ => {}
    }
}

fn main() {}
