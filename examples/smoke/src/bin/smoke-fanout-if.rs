//! Rooted at `pipeline_fanout_if`. Golden pins `.fan_out` INSIDE an
//! `if` arm: the arm sub-workflow's aggregate consumer must carry the
//! kind-aware `FanAgg` re-norm expr, not the plain (double-encoding)
//! parameter ref — the re-tag pass used to skip arm scopes.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_fanout_if);
}
