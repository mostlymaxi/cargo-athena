//! Emits / runs the cross-module + cross-crate composition rooted at
//! `importing_pipeline`. Because this binary force-links the smoke crate
//! via the wormhole, it can also run an upstream container in-process:
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-smoke-transform \
//!   CARGO_ATHENA_INPUT='{"data":"x","factor":2}' ... --bin importing

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_importing::importing_pipeline>();
}
