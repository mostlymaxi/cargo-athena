// A #[fragment] is NOT a template — it stays a plain `fn`, not a `Template`
// type. Calling one from a #[workflow] therefore can't compile (the
// generated `<frag as Template>` use fails). Same guarantee covers any
// regular (un-annotated) function call from a #[workflow].
#[cargo_athena::fragment]
fn frag() {}

#[cargo_athena::workflow]
fn wf() {
    frag();
}

fn main() {}
