// Only String / str / numbers are injectable (their serde form
// round-trips to the obvious raw scalar) — a Vec<u8> is not.
#[cargo_athena::container(image = "repo:" + blob)]
fn c(blob: Vec<u8>) {
    let _ = blob;
}

fn main() {}
