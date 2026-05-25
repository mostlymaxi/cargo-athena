//! Rooted at `pipeline_mutex`. Golden pins template-level
//! `Template.synchronization.mutexes` (injection from
//! `inputs.parameters['shard']`) and root-only
//! `WorkflowSpec.synchronization.mutexes` (injection from
//! `workflow.parameters['env']`).

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_mutex>();
}
