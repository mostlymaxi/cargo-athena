//! Emits / runs the broad fixture rooted at `pipeline`.
//!
//!   cargo run -p cargo-athena-example-e2e --bin e2e        # emit multi-doc YAML
//!   CARGO_ATHENA_TEMPLATE=<name> ... --bin e2e             # run one container

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_e2e::pipeline>();
}
