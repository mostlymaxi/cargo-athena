// `#[inject]` on an `Artifact<..>` arg is rejected: artifact inputs
// are wired from upstream tasks, not Argo expressions — previously the
// attr was silently dropped, leaving an input artifact with no source.
#[cargo_athena::container]
fn c(#[inject("{{workflow.name}}")] data: cargo_athena::Artifact<String>) -> String {
    let _ = data;
    String::new()
}

fn main() {}
