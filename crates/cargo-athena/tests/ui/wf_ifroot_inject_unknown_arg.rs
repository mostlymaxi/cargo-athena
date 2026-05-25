// `node_selector_if_root` injection: `"lit" + ident` requires `ident`
// to be one of this `#[workflow]`'s args (substituted via
// `workflow.parameters.<ident>` at submission). Referencing an
// unknown name is a targeted compile error — same rule as
// `#[container]` injection.
#[cargo_athena::workflow(node_selector_if_root = { "env" = "prod-" + missing })]
fn pipeline(env: String) {}

fn main() {}
