//! Rooted at `pipeline_deadline`. Golden pins per-template
//! `Template.activeDeadlineSeconds` from `#[…(active_deadline = …)]`
//! (both the integer-seconds and humantime-string forms).

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_deadline>();
}
