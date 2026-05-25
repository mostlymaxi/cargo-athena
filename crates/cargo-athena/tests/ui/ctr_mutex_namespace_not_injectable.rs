// `mutexes[].namespace` injection follows the same `Injectable` rules
// as `name` — a `Vec<u8>` arg has no raw-scalar fromJSON form, so the
// hidden `__athena_inject_check` shim fails to type-check.
#[cargo_athena::container(mutexes = [{ name = "lock", namespace = "ns-" + blob }])]
fn c(blob: Vec<u8>) {
    let _ = blob;
}

fn main() {}
