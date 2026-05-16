// `host!` in a plain (un-annotated) fn must not compile: the collector
// can't see it, so a silently-unmounted path is impossible.
fn helper() {
    let _ = cargo_athena::host!("/nope");
}

fn main() {
    helper();
}
