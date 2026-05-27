//! Rooted at `pipeline_image_pull_secrets`. Golden pins root-only
//! `WorkflowSpec.ImagePullSecrets` (two literal Secret names).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_image_pull_secrets);
}
