// `mutexes[].name` injection must reference one of the #[container]'s
// own args — `nope` isn't a parameter, so this is a hard error
// (same machinery as image/env injection).
#[cargo_athena::container(mutexes = [{ name = "lock-" + nope }])]
fn c(shard: String) {
    let _ = shard;
}

fn main() {}
