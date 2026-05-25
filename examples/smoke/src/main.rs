//! Emits / runs the broad "all features" fixture rooted at `pipeline`.
//!
//!   cargo run -p cargo-athena-example-smoke --bin smoke   # emit multi-doc YAML
//!   CARGO_ATHENA_TEMPLATE=<name> ... --bin smoke          # run one container

fn main() {
    cargo_athena::entrypoint!(cargo_athena_example_smoke::pipeline);
}
