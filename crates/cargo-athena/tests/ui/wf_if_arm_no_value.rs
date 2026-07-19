// A value-`if` arm that doesn't produce the value errors with the
// ARM's own span (it used to be clobbered to the fn's return type by
// error-message text matching).
#[cargo_athena::container]
fn val() -> String {
    String::new()
}

#[cargo_athena::container]
fn act() {}

#[cargo_athena::workflow]
fn wf(flag: String) -> String {
    let x = if flag == "a" {
        act();
    } else {
        val()
    };
    x
}

fn main() {}
