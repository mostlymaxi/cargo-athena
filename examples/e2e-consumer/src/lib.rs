//! A separate crate that composes the upstream `cargo-athena-example-e2e`
//! fixture's `pipeline` with its own local templates — *exactly* the same
//! way an intra-crate workflow composes a local one. If the type-wormhole
//! leaked across the crate boundary, `consumer_pipeline`'s emitted stream
//! would be missing every `cargo-athena-example-e2e-*` template and the
//! golden would fail.

use cargo_athena::{container, workflow};
// Cross-crate import of an upstream workflow — used like a local one.
use cargo_athena_example_e2e::pipeline;

#[container]
pub fn consumer_step(note: String) -> String {
    format!("consumed:{note}")
}

#[container]
pub fn finalize(x: String) {
    println!("finalize {x}");
}

#[workflow]
pub fn consumer_pipeline() {
    let n = consumer_step("hello".to_string());
    pipeline(); // cross-crate workflow -> workflow (the wormhole)
    finalize(n); // depends on consumer_step's output
}
