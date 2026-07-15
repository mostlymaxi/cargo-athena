// `#[inject]` args must be trailing: INPUTS carries every arg in
// signature order while the caller-visible signature drops inject
// args, so a normal parameter after an inject arg would be wired to
// the wrong input name (compiles clean, breaks at submit).
#[cargo_athena::container]
fn c(#[inject("{{retries}}")] attempt: i64, payload: String) -> String {
    format!("{attempt}:{payload}")
}

fn main() {}
