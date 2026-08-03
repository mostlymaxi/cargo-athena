// A `.continue_on` binding can't be the workflow's return value.
#[cargo_athena::container]
fn fetch() -> String {
    "v".to_string()
}

#[cargo_athena::workflow]
fn wf() -> Result<String, cargo_athena::ArgoError> {
    let r = fetch().continue_on(failed);
    r
}

fn main() {}
