//! In-process module/smoke tests for the facade: emit semantics asserted
//! via `Collector`/`Template` without spawning any example binary. This is
//! the home for "does the compiler lower things right" coverage (it
//! supersedes the old `examples/basic` inline tests and adds more).

use cargo_athena::{BuildCtx, Collector, Template, container, fragment, workflow};

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
    format!("leaf:{tag}:{}", cfg.display())
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
    // returns a value: tail call -> this workflow's outputs.parameters.return
    prep(seed)
}

#[workflow]
fn root() {
    let a = inner("seed".to_string()); // nested workflow that RETURNS
    let b = leaf(a); // consumes {{tasks.a.outputs.parameters.return}}
    sink(b);
}

#[workflow(steps)]
fn seq() {
    let p = prep("x".to_string());
    sink(p);
}

fn emit<T: Template>() -> String {
    emit_tagged::<T>(None)
}

/// Emit with an optional build-time version tag, as `cargo athena build`
/// would bake via `ATHENA_VERSION_TAG`. `None` = plain build (tag falls
/// back to `kebab(CARGO_PKG_VERSION)`); `Some("dev-foo")` simulates a dev
/// build. The substring assertions in the no-tag tests are
/// version-suffix-tolerant (they match `name: <base>` as a prefix).
fn emit_tagged<T: Template>(tag: Option<&str>) -> String {
    let mut c = Collector::new();
    <T as Template>::collect(&mut c);
    // These assertions cover the runnable Workflow too, so emit it.
    let ctx = BuildCtx::collect(
        env!("CARGO_PKG_NAME"),
        env!("CARGO_PKG_VERSION"),
        env!("CARGO_PKG_NAME"),
        tag,
        None,
        None,
    );
    c.emit::<T>(&ctx, true)
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
    // A returning #[workflow] declares its own outputs.parameters.return
    // via valueFrom.parameter (NOT a container's valueFrom.path, and NOT
    // the bare `outputs.result` script-stdout alias).
    assert!(inner_wt.contains("outputs:"), "no outputs:\n{inner_wt}");
    assert!(
        inner_wt.contains("parameter: '{{tasks.")
            && inner_wt.contains(".outputs.parameters.return}}'"),
        "expected valueFrom.parameter task ref:\n{inner_wt}"
    );
}

#[test]
fn version_tag_overlays_cluster_identity_only() {
    // A dev build's baked tag (`cargo athena build --dev-tag foo`).
    let yaml = emit_tagged::<root>(Some("dev-foo"));
    // Bound to non-colliding names (the `#[container] fn leaf` /
    // `#[workflow] fn root` unit structs share these idents).
    let leaf_base = <leaf as Template>::ARGO_NAME;
    let root_base = <root as Template>::ARGO_NAME;

    // metadata.name + every templateRef.name (the cluster-resource
    // pointers) carry the tag suffix; the runnable Workflow points at
    // the versioned root.
    assert!(
        yaml.contains(&format!("name: {leaf_base}-dev-foo")),
        "leaf WT / templateRef.name not versioned:\n{yaml}"
    );
    assert!(
        yaml.contains(&format!("name: {root_base}-dev-foo")),
        "root WT not versioned"
    );
    assert!(yaml.contains("workflowTemplateRef:"));

    // Baked provenance labels.
    assert!(
        yaml.contains("cargo.athena/tag: dev-foo"),
        "missing tag label"
    );
    assert!(
        yaml.contains("cargo.athena/channel: dev"),
        "dev tag must derive the dev channel"
    );

    // INVARIANT: entrypoint, the inner `templates[].name`, and every
    // `templateRef.template` stay on the BASE name (the closed
    // base-name world that keeps in-pod dispatch + Argo's within-WT
    // lookup working). A trailing `\n` pins the base-only form so the
    // versioned form can't match as a prefix.
    assert!(
        yaml.contains(&format!("entrypoint: {leaf_base}\n")),
        "entrypoint must stay base:\n{yaml}"
    );
    assert!(
        !yaml.contains(&format!("entrypoint: {leaf_base}-dev-foo")),
        "entrypoint was wrongly versioned"
    );
    assert!(
        yaml.contains(&format!("template: {leaf_base}\n")),
        "templateRef.template should be the base name:\n{yaml}"
    );
    assert!(
        !yaml.contains(&format!("template: {leaf_base}-dev-foo")),
        "templateRef.template must NOT be versioned:\n{yaml}"
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
    assert!(yaml.contains("{{steps.p.outputs.parameters.return}}"));
}
