// A `.continue_on` binding is `Result<T, ArgoError>`; a consumer
// declaring plain `T` must not compile.
#[cargo_athena::container]
fn fetch() -> String {
    "v".to_string()
}

#[cargo_athena::container]
fn publish(x: String) {
    println!("{x}");
}

#[cargo_athena::workflow]
fn wf() {
    let r = fetch().continue_on(failed);
    publish(r);
}

fn main() {}
