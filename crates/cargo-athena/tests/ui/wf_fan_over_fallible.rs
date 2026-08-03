// A `.continue_on` list binding is a `Result`, not a list — handle the
// error in a container first, then fan out over its output.
#[cargo_athena::container]
fn make() -> Vec<String> {
    vec![]
}

#[cargo_athena::container]
fn upper(x: String) -> String {
    x.to_uppercase()
}

#[cargo_athena::container]
fn done(xs: Vec<String>) {
    println!("{xs:?}");
}

#[cargo_athena::workflow]
fn wf() {
    let list = make().continue_on(failed);
    let out = list.fan_out(|x| upper(x));
    done(out);
}

fn main() {}
