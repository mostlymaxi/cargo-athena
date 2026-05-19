//! Rooted at `pipeline_deadline`. Golden pins both timeouts: per-pod
//! `Template.activeDeadlineSeconds` from `#[container(pod_running_timeout
//! = …)]` and root-only `WorkflowSpec.activeDeadlineSeconds` from
//! `#[…(active_deadline_if_root = …)]` (int-seconds + humantime forms).

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_deadline>();
}
