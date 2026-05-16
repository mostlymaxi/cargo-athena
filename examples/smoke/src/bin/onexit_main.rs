//! Rooted at `pipeline_onexit`. Golden pins `Workflow.spec.onExit` (from
//! `#[workflow(on_exit=…)]`, root-only) and an `.on_exit(record("done"))`
//! exit hook *with arguments*.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_onexit>();
}
