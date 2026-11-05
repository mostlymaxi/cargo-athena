// Constant values in #[workflow] args: the macro's targeted errors.
#[cargo_athena::container]
fn take(s: String) {
    println!("{s}");
}

#[cargo_athena::container]
fn take_n(count: u32) {
    println!("{count}");
}

#[cargo_athena::container]
fn take_pair(p: (u32, u32)) {
    println!("{p:?}");
}

#[cargo_athena::container]
fn take_v(v: Vec<u32>) {
    println!("{v:?}");
}

#[cargo_athena::container]
fn mk() -> u32 {
    1
}

#[cargo_athena::container]
fn list() -> Vec<u32> {
    vec![]
}

#[allow(dead_code)]
struct Opts {
    a: u32,
}

const MAX: u32 = 3;
const NAME: &str = "x";

// (a) an input inside a struct literal
#[cargo_athena::workflow]
fn input_in_struct(k: u32) {
    take_n(Opts { a: k }.a);
}

// (b) a task binding inside a tuple
#[cargo_athena::workflow]
fn task_in_tuple() {
    let t = mk();
    take_pair((t, 1));
}

// (c) the fan_out item inside a struct literal
#[cargo_athena::workflow]
fn item_in_struct() {
    let l = list();
    l.fan_out(|x| take_n(Opts { a: x }.a));
}

// (d) a non-constructor call
#[cargo_athena::workflow]
fn plain_call() {
    take_n(Opts { a: compute() }.a);
}

// (e) `.into()` on a path
#[cargo_athena::workflow]
fn into_on_path() {
    take(NAME.into());
}

// (f) arithmetic
#[cargo_athena::workflow]
fn arithmetic() {
    take_n(MAX * 2);
}

// (g) a macro
#[cargo_athena::workflow]
fn macro_value() {
    take_v(vec![1]);
}

// (h) a `const` item in the body
#[cargo_athena::workflow]
fn const_in_body() {
    const N: u32 = 3;
    take_n(N);
}

// (i) a `let` alias of an input
#[cargo_athena::workflow]
fn alias(k: u32) {
    let m = k;
    take_n(m);
}

// (j) `fan_out` over a constant
#[cargo_athena::workflow]
fn fan_const() {
    let l = [1, 2];
    l.fan_out(|x| take_n(x));
}

// (k) a nested template call in a hook arg
#[cargo_athena::workflow]
fn nested_in_hook() {
    take_n(1).on_exit(take_n(mk()));
}

// (l) a type annotation on a constant `let`
#[cargo_athena::workflow]
fn typed_let() {
    let big: u64 = 5_000_000_000;
    take(big.to_string());
}

// (m) returning a constant `let`
#[cargo_athena::workflow]
fn returns_const() -> u32 {
    let k = 3;
    k
}

// (n) an error on a constant `let` use points at the use
#[cargo_athena::workflow]
fn const_use() {
    let k = 3;
    take_n(k * 2);
}

fn compute() -> u32 {
    1
}

fn main() {}
