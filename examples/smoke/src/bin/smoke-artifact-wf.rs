//! Rooted at `pipeline_artifact_wf`. Golden pins workflow-side
//! `Artifact<T>` flow: a sub-workflow that RETURNS `Artifact<Meta>`
//! (bubbled via `outputs.artifacts.return.from`) consumed by a sibling
//! sub-workflow that ACCEPTS `Artifact<Meta>` as input (forwarded via
//! `inputs.artifacts.<name>` -> downstream container).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_artifact_wf);
}
