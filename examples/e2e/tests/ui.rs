//! Compile-fail contract: `host!` is rejected anywhere the resource
//! collector can't see it. Refresh expected output with
//! `TRYBUILD=overwrite cargo test -p cargo-athena-example-e2e --test ui`.

#[test]
fn host_is_gated_outside_container_or_fragment() {
    let t = trybuild::TestCases::new();
    t.compile_fail("tests/ui/*.rs");
}
