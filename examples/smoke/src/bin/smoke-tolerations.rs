//! Rooted at `pipeline_tolerations`. Golden pins template-level
//! `Template.Tolerations` (injection from `inputs.parameters['kind']`)
//! AND root-only `WorkflowSpec.Tolerations` (injection from
//! `workflow.parameters['role']`).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_tolerations);
}
