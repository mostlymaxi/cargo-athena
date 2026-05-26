//! Rooted at `pipeline_affinity`. Golden pins template-level
//! `Template.Affinity` (opaque YAML preference) AND root-only
//! `WorkflowSpec.Affinity` (required-nodeAffinity with embedded
//! `{{workflow.parameters.role}}`).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_affinity);
}
