//! Emits / runs the cross-crate composition rooted at `consumer_pipeline`.
//! Because this binary force-links the e2e crate via the wormhole, it can
//! also run an upstream container in-process, e.g.:
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-e2e-transform \
//!   CARGO_ATHENA_INPUT='{"data":"x","factor":2}' ... --bin e2e-consumer

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_e2e_consumer::consumer_pipeline>();
}
