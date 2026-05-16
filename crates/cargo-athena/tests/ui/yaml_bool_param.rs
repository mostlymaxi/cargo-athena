// An argument whose name a YAML 1.1 parser (Argo's Go YAML→JSON) reads
// as a boolean/null must be a hard compile error — never a silently
// mis-typed emitted workflow.
#[cargo_athena::container]
fn produce(n: i64) -> i64 {
    n
}

fn main() {}
