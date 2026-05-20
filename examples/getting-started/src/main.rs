//! Tiny pipeline: `fetch -> summarize -> publish`. Data flow becomes the DAG.
//!
//!   cargo run -p cargo-athena-example-getting-started > emit.yaml

use cargo_athena::{container, workflow};

#[workflow]
fn pipeline() {
    let raw = fetch("https://example.com/data".to_string());
    let summary = summarize(raw, 3);
    publish(summary);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn fetch(url: String) -> String {
    format!("data-from:{url}")
}

#[container]
fn summarize(data: String, top_n: i64) -> String {
    format!("top-{top_n}:{data}")
}

#[container]
fn publish(report: String) {
    println!("publishing {report}");
}

fn main() {
    cargo_athena::entrypoint::<pipeline>();
}
