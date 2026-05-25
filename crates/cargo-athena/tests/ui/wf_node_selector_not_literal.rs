// `boundary_node_selector` takes **literal strings only** (keys *and*
// values). Per-arg injection here would have to lower to
// `workflow.parameters` (root-scoped) and silently mis-resolve when
// this WT is templateRef'd — keep boundary selectors static. Use
// `node_selector_if_root` (a separate attr) for dynamic values.
#[cargo_athena::workflow(boundary_node_selector = { "disktype" = profile })]
fn pipeline(profile: String) {
    let _ = profile;
}

fn main() {}
