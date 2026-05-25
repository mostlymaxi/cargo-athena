//! Two containers that emit `tracing::info!` records. The subscriber
//! is installed once in `main.rs`, gated on `is_container_run()` so
//! it only fires in-pod.

use cargo_athena::{container, workflow};

#[workflow]
pub fn pipeline() {
    let s = greet("athena".to_string());
    shout(s);
}

#[container]
pub fn greet(name: String) -> String {
    // These flow through the subscriber installed in `main.rs` only when
    // running in-pod. Locally via `cargo athena emit` they never fire,
    // because the body never executes.
    tracing::info!(name = %name, "greeting");
    format!("hello, {name}")
}

#[container]
pub fn shout(msg: String) {
    tracing::info!(msg = %msg, "shouting");
    println!("{}", msg.to_uppercase());
}
