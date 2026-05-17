// Unlike `#[container]`, a `#[workflow]` node_selector is *literal
// strings only* — no `"lit" + arg` parameter injection. A non-literal
// value (here a bare arg) is a hard error: a workflow is a DAG, not a
// pod, and only a raw `{{workflow.parameters.X}}` literal can be
// dynamic (the documented escape hatch).
#[cargo_athena::workflow(node_selector = { "disktype" = profile })]
fn pipeline(profile: String) {
    let _ = profile;
}

fn main() {}
