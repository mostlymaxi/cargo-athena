//! Compile-fail contracts for the proc macros:
//!   * `host!` rejected where the resource collector can't see it,
//!   * non-(de)serializable `#[container]` I/O,
//!   * strict `#[workflow]` body: only template calls — loops/macros/
//!     method-calls/unsupported args/unresolved returns are hard errors,
//!   * `#[fragment]`/regular fns aren't `Template`s (type-system gate).
//!
//! Refresh expected output with:
//!   TRYBUILD=overwrite cargo test -p cargo-athena --test ui

#[test]
fn compile_fail_contracts() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
