//! Rooted at `pipeline_daemon`. Golden pins `Template.daemon: true` from
//! `#[container(daemon)]` (and its absence on the plain container + the
//! workflow's dag template).

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline_daemon);
}
