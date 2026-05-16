//! Minimal example on the new (type-identity) API.
//!
//!   cargo run -p cargo-athena-example-basic
//!     -> emits a multi-doc WorkflowTemplate stream rooted at `run_foo`
//!
//!   CARGO_ATHENA_TEMPLATE=cargo-athena-example-basic-run-a-container \
//!   CARGO_ATHENA_INPUT='{"a":"hi"}' cargo run -p cargo-athena-example-basic
//!     -> runs that container's real body in-process

// `host!` is context-restricted; always called path-qualified.
use cargo_athena::{container, fragment, workflow};

#[workflow]
fn run_foo() {
    let a = some_other_workflow("asdf".to_string());
    run_a_container(a);
}

#[workflow]
fn some_other_workflow(b: String) {
    let p = prepare(b);
    finalize(p);
}

#[container(image = "ghcr.io/acme/app:latest")]
fn run_a_container(a: String) {
    let cfg = cargo_athena::host!("/etc/myapp");
    load_extra(); // cross-item: pulls /var/lib/extra onto this template
    println!("config dir: {cfg}");
    println!("this is regular code, got: {a}");
}

#[container]
fn prepare(b: String) -> String {
    format!("prepared:{b}")
}

#[container]
fn finalize(p: String) {
    println!("final: {p}");
}

#[fragment]
fn load_extra() {
    let _extra = cargo_athena::host!("/var/lib/extra");
}

fn main() {
    cargo_athena::entrypoint::<run_foo>();
}

#[cfg(test)]
mod tests {
    use cargo_athena::{Collector, Template};

    #[test]
    fn emits_expected_templates() {
        let mut c = Collector::new();
        <crate::run_foo as Template>::collect(&mut c);
        let yaml = c.emit::<crate::run_foo>();

        // One WorkflowTemplate per template, namespaced by crate.
        assert!(yaml.contains("kind: WorkflowTemplate"));
        assert!(yaml.contains("name: cargo-athena-example-basic-run-foo"));
        assert!(yaml.contains("name: cargo-athena-example-basic-some-other-workflow"));

        // Cross-item host! closure landed on the container template.
        assert!(yaml.contains("/etc/myapp"));
        assert!(yaml.contains("/var/lib/extra"));

        // Cross-template calls are by reference, and a runnable Workflow
        // for the entrypoint is appended.
        assert!(yaml.contains("templateRef:"));
        assert!(yaml.contains("kind: Workflow\n"));
        assert!(yaml.contains("workflowTemplateRef:"));
    }
}
