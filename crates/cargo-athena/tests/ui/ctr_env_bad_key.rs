// `env` keys become k8s env-var names (`[-._a-zA-Z][-._a-zA-Z0-9]*`);
// a space would fail at pod admission — reject at compile time.
#[cargo_athena::container(env = { "BAD KEY" = "v" })]
fn f() {}

fn main() {}
