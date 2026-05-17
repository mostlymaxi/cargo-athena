// Attribute values are string literals or `+`-concat of literals and
// args / named fields — a method call is a targeted error.
#[cargo_athena::container(image = "repo:" + tag.len())]
fn c(tag: String) {
    let _ = tag;
}

fn main() {}
