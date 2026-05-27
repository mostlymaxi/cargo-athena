//! Rooted at `pipeline_boundary_scheduling`. Golden pins
//! `Template.Tolerations` + `Template.Affinity` on the dag template
//! (boundary tier - inherited by child pods without their own).
//! Literal-only.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_boundary_scheduling);
}
