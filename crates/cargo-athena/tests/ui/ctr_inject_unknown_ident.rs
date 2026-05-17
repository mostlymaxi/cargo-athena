// An injected ident must be one of the #[container]'s own arguments.
#[cargo_athena::container(image = "repo:" + nope)]
fn c(tag: String) {
    let _ = tag;
}

fn main() {}
