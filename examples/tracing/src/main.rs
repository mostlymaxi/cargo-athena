//! Demonstrates `is_container_run()`: gate one-time setup (a tracing
//! subscriber) so it fires only when Argo is dispatching a container
//! body in-pod -- not on every `cargo athena emit` / `ls` / `describe`
//! / `submit` invocation, which would otherwise also spawn this
//! binary to introspect templates.
//!
//! Try it both ways:
//!
//!   cargo run -p cargo-athena-example-tracing
//!     -> emits the WorkflowTemplate YAML; NO tracing setup runs.
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-tracing-greet \
//!     cargo run -p cargo-athena-example-tracing -- '"athena"'
//!     -> runs `greet` in-process; tracing IS initialized, you see
//!        the `INFO greeting{name=athena}` line.

fn main() {
    // Only initialize the subscriber in-pod. The returned guard (here
    // just `()`) drops at the end of main(); if your setup needs
    // flush-on-exit, return a real Drop type instead.
    let _gate = cargo_athena::is_container_run().then(|| {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .init();
    });

    cargo_athena::entrypoint!(cargo_athena_example_tracing::pipeline);
}
