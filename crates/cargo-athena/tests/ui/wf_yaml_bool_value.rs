// A literal arg value that is a YAML 1.1 boolean (`no`, here) would be
// emitted as a bare scalar and rejected by Argo's YAML→JSON parser
// (`must be of type string`). Fail loud at compile time — `true`/`false`
// stay usable (serde_norway auto-quotes them).
#[cargo_athena::container]
fn act(flag: String) {
    let _ = flag;
}

#[cargo_athena::workflow]
fn wf() {
    act("no".to_string());
}

fn main() {}
