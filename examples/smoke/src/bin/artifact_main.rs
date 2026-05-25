//! Rooted at `pipeline_artifact`. Golden pins producer-side
//! `outputs.artifacts.return` (with templated `s3.key`) and consumer-
//! side `arguments.artifacts.return.from` wiring across the templateRef
//! wormhole. Live-validated on Argo v4.0.5 in the probe step.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_artifact);
}
