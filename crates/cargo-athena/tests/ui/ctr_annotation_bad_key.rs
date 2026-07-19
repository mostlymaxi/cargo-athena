// `annotations` keys are k8s qualified names ([prefix/]name); a `!`
// would fail at admission — reject at compile time.
#[cargo_athena::container(annotations = { "bad!key" = "v" })]
fn f() {}

fn main() {}
