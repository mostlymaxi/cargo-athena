// `secret!` is only valid inside #[container] or #[fragment] (same
// gating as host!/load_artifact!/save_artifact!). Outside those, the
// public form is a hard compile_error so a silently-missing env var
// can't reach a pod.
fn main() {
    let _ = cargo_athena::secret!("api-tokens", "api");
}
