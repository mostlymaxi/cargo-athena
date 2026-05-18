//! Rooted at `pipeline_retry`. Golden pins template-level
//! `retryStrategy`/`timeout` from `#[container(retry(..), timeout=…)]`
//! and `#[workflow(retry(..))]`.

fn main() {
    cargo_athena::entrypoint::<cargo_athena_example_smoke::pipeline_retry>();
}
