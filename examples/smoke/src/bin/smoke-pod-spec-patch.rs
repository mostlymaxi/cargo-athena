//! Rooted at `pipeline_pod_spec_patch`. Golden pins template-level
//! `Template.PodSpecPatch` (injection from
//! `inputs.parameters['cpu_limit']`) and root-only
//! `WorkflowSpec.PodSpecPatch` (injection from
//! `workflow.parameters['grace']`).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_pod_spec_patch);
}
