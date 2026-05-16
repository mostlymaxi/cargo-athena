// A #[container] with a non-(de)serializable input/output MUST fail to
// compile: the generated run-wrapper instantiates the serde bounds even
// though it is never called, so this can never silently pass at runtime.
struct NotSerde {
    _x: i32,
}

#[cargo_athena::container]
fn bad(v: NotSerde) -> NotSerde {
    v
}

fn main() {}
