//! In-process module/smoke tests for the facade: emit semantics asserted
//! via `Collector`/`Template` without spawning any example binary. This is
//! the home for "does the compiler lower things right" coverage (it
//! supersedes the old `examples/basic` inline tests and adds more).

use cargo_athena::{container, fragment, workflow, Collector, Template};

// --- a representative graph ------------------------------------------------

#[fragment]
fn carry() {
    // cross-item: this host! must land on `leaf`'s template even though
    // `leaf` declares its own.
    let _ = cargo_athena::host!("/var/lib/carry");
}

#[container(image = "ghcr.io/acme/leaf:1")]
fn leaf(tag: String) -> String {
    let cfg = cargo_athena::host!("/etc/leaf");
    carry();
    format!("leaf:{tag}:{cfg}")
}

#[container]
fn prep(seed: String) -> String {
    format!("prep:{seed}")
}

#[container]
fn sink(v: String) {
    println!("sink {v}");
}

#[workflow]
fn inner(seed: String) -> String {
    // returns a value: tail container call -> this workflow's outputs.result
    prep(seed)
}

#[workflow]
fn root() {
    let a = inner("seed".to_string()); // nested workflow that RETURNS
    let b = leaf(a); // consumes {{tasks.a.outputs.result}}
    sink(b);
}

#[workflow(steps)]
fn seq() {
    let p = prep("x".to_string());
    sink(p);
}

fn emit<T: Template>() -> String {
    let mut c = Collector::new();
    <T as Template>::collect(&mut c);
    c.emit::<T>()
}

#[test]
fn one_workflowtemplate_per_template_plus_runnable_workflow() {
    let yaml = emit::<root>();
    // Each template is its own WorkflowTemplate, addressed by its
    // compiler-resolved ARGO_NAME (crate-namespaced, collision-proof).
    for t in [
        <root as Template>::ARGO_NAME,
        <inner as Template>::ARGO_NAME,
        <leaf as Template>::ARGO_NAME,
        <prep as Template>::ARGO_NAME,
        <sink as Template>::ARGO_NAME,
    ] {
        assert!(
            yaml.contains(&format!("name: {t}")),
            "missing template {t} in:\n{yaml}"
        );
    }
    assert!(yaml.contains("kind: WorkflowTemplate"));
    // Cross-template calls are by reference; a runnable Workflow for the
    // entrypoint is appended.
    assert!(yaml.contains("templateRef:"));
    assert!(yaml.contains("kind: Workflow\n"));
    assert!(yaml.contains("workflowTemplateRef:"));
}

#[test]
fn cross_item_host_closure_lands_on_container() {
    let yaml = emit::<root>();
    // `leaf`'s own host! AND the transitive `#[fragment] carry` host!.
    assert!(yaml.contains("/etc/leaf"), "leaf host! missing:\n{yaml}");
    assert!(
        yaml.contains("/var/lib/carry"),
        "fragment host! closure missing:\n{yaml}"
    );
}

#[test]
fn workflow_return_value_bubbles_outputs_result() {
    let yaml = emit::<root>();
    let inner_wt = yaml
        .split("---")
        .find(|d| d.contains(&format!("name: {}", <inner as Template>::ARGO_NAME)))
        .expect("inner WorkflowTemplate");
    // A returning #[workflow] declares its own outputs.parameters.result
    // via valueFrom.parameter (NOT a container's valueFrom.path).
    assert!(inner_wt.contains("outputs:"), "no outputs:\n{inner_wt}");
    assert!(
        inner_wt.contains("parameter: '{{tasks.")
            && inner_wt.contains(".outputs.result}}'"),
        "expected valueFrom.parameter task ref:\n{inner_wt}"
    );
}

#[test]
fn steps_mode_emits_sequential_steps_not_dag() {
    let yaml = emit::<seq>();
    let wt = yaml
        .split("---")
        .find(|d| d.contains(&format!("name: {}", <seq as Template>::ARGO_NAME)))
        .expect("seq WorkflowTemplate");
    assert!(wt.contains("steps:"), "expected steps body:\n{wt}");
    assert!(!wt.contains("dag:"), "steps mode must not emit dag:\n{wt}");
    assert!(yaml.contains("{{steps.p.outputs.result}}"));
}
