// Unlike `#[container]`, both `#[workflow(boundary_node_selector)]`
// and `#[workflow(node_selector_if_root)]` take **literal strings
// only** — no `"lit" + arg` parameter injection. A workflow has no
// args to inject from; the only documented escape hatch is a raw
// `{{workflow.parameters.X}}` literal value.
#[cargo_athena::workflow(boundary_node_selector = { "disktype" = profile })]
fn pipeline(profile: String) {
    let _ = profile;
}

fn main() {}
