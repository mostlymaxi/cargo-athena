//! Rooted at `pipeline_ns`. Golden pins `#[workflow(node_selector=…)]`:
//! literal key+value set on the dag template (the Argo controller
//! cascades it onto every `templateRef`'d task pod), including the raw
//! `{{workflow.parameters.region}}` escape-hatch literal.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_ns);
}
