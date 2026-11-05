// `if`-condition comparisons are typed by the ghost: only strings, chars,
// bools and numbers compile. A set's JSON order, a struct's own
// `PartialEq`, `bool` ordering or a nested `Option` (`Some(None)` is
// `null`) would silently differ in Argo.
use std::collections::HashSet;
use std::time::Duration;

#[cargo_athena::container]
fn tags() -> HashSet<String> {
    HashSet::new()
}

#[cargo_athena::container]
fn wait() -> Duration {
    Duration::ZERO
}

#[cargo_athena::container]
fn flag() -> bool {
    false
}

#[cargo_athena::container]
fn act() {}

#[cargo_athena::workflow]
fn set_eq() {
    let a = tags();
    let b = tags();
    if a == b {
        act();
    }
}

#[cargo_athena::workflow]
fn struct_ne() {
    let a = wait();
    let b = wait();
    if a != b {
        act();
    }
}

#[cargo_athena::workflow]
fn bool_ord() {
    let f = flag();
    if f < true {
        act();
    }
}

#[cargo_athena::container]
fn owner() -> Option<Option<String>> {
    None
}

#[cargo_athena::workflow]
fn option_eq() {
    let a = owner();
    let b = owner();
    if a == b {
        act();
    }
}

fn main() {}
