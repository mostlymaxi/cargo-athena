// Sub-second durations can't be represented (Argo counts whole
// seconds) — a targeted error, not the generic parse message.
#[cargo_athena::container(timeout = "500ms")]
fn f() {}

fn main() {}
