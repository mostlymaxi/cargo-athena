//! Rooted at `pipeline_artifact_clone`. Golden pins `.clone()` on an
//! `Artifact<T>` binding as a fan-out marker: two downstream consumers
//! both `from:` the same upstream artifact, no in-pod clone.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_artifact_clone);
}
