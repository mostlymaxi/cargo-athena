// `timeout` is `#[container]`-only: Argo applies `Template.timeout`
// to neither dag nor steps templates, so it is not an accepted
// `#[workflow]` attribute (the whole-workflow cap is
// `active_deadline_if_root`). deluxe rejects the unknown field.
#[cargo_athena::workflow(timeout = "5m")]
fn f() {}

fn main() {}
