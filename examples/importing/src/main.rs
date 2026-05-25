//! Emits / runs the cross-module + cross-crate composition rooted at
//! `importing_pipeline`. Because this binary force-links the smoke crate
//! via the wormhole, it can also run an upstream container in-process:
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-smoke-transform \
//!     ... --bin importing -- '"x"' 2
//!   (positional argv: `transform(data: String, factor: i64)`)

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_importing::importing_pipeline);
}
