//! Rooted at `pipeline_onexit`. Golden pins the template's own
//! `spec.hooks.exit` (from `#[workflow(on_exit_if_root=…)]`) and a
//! per-task `.on_exit(record("done"))` hook *with arguments*.

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_onexit);
}
