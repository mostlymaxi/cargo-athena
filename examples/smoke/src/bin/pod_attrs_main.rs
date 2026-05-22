//! Rooted at `pipeline_pod_attrs`. Pins the emit shape for the three
//! pod-spec attrs (`env`, `host_mount`, `annotations`) plus the
//! workflow-level annotations on the dag template.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_pod_attrs>();
}
