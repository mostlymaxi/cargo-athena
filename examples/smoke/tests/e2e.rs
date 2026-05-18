//! Golden: spawn the compiled `smoke` bins and pin their emit/run output.
//!
//!   cargo test -p cargo-athena-example-smoke                 # assert
//!   UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke # refresh goldens

use std::path::PathBuf;
use std::process::Command;

const BIN_PIPELINE: &str = env!("CARGO_BIN_EXE_smoke");
const BIN_RETURNS: &str = env!("CARGO_BIN_EXE_smoke-returns");
const BIN_HOOKS: &str = env!("CARGO_BIN_EXE_smoke-hooks");
const BIN_ONEXIT: &str = env!("CARGO_BIN_EXE_smoke-onexit");
const BIN_FIELDS: &str = env!("CARGO_BIN_EXE_smoke-fields");
const BIN_FANOUT: &str = env!("CARGO_BIN_EXE_smoke-fanout");
const BIN_IF: &str = env!("CARGO_BIN_EXE_smoke-if");
const BIN_NESTED: &str = env!("CARGO_BIN_EXE_smoke-nested");
const BIN_INJECT: &str = env!("CARGO_BIN_EXE_smoke-inject");
const BIN_NS: &str = env!("CARGO_BIN_EXE_smoke-ns");

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

/// Compare `actual` against `tests/golden/<name>`, or rewrite it when
/// `UPDATE_EXPECT` is set.
fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_EXPECT").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        eprintln!("updated golden {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {} — run `UPDATE_EXPECT=1 cargo test`",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "\n{name} drifted from its golden. If intended, refresh with \
         `UPDATE_EXPECT=1 cargo test -p cargo-athena-example-smoke`.\n"
    );
}

fn run_bin(bin: &str, envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(bin);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to spawn smoke binary");
    assert!(
        out.status.success(),
        "{bin} exited with {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("binary stdout was not UTF-8")
}

/// Emit mode: the multi-doc WorkflowTemplate stream for `pipeline`
/// (templates only — the default).
#[test]
fn emit_pipeline() {
    assert_golden("pipeline.yaml", &run_bin(BIN_PIPELINE, &[]));
}

/// `--with-workflow`: the same stream **plus** the convenience runnable
/// `Workflow` (generateName, workflowTemplateRef → root).
#[test]
fn emit_pipeline_with_workflow() {
    assert_golden(
        "pipeline_with_workflow.yaml",
        &run_bin(BIN_PIPELINE, &[("CARGO_ATHENA_WITH_WORKFLOW", "1")]),
    );
}

/// `#[workflow]` return values: `sub_pipeline` (returns its tail call's
/// result) is consumed by `pipeline_returns`. Pins the emitted
/// `outputs.parameters.result` block + `{{tasks.r.outputs.result}}` wiring.
#[test]
fn emit_pipeline_returns() {
    assert_golden("pipeline_returns.yaml", &run_bin(BIN_RETURNS, &[]));
}

/// Per-task builders: pins `continueOn`, the `exit` hook, and an
/// expression hook on the emitted DAG tasks (+ hook templates emitted).
#[test]
fn emit_pipeline_hooks() {
    assert_golden("pipeline_hooks.yaml", &run_bin(BIN_HOOKS, &[]));
}

/// `#[workflow(on_exit_if_root=…)]` -> the template's own
/// `spec.hooks.exit`, plus a per-task `.on_exit(record("done"))` hook
/// carrying arguments.
#[test]
fn emit_pipeline_onexit() {
    assert_golden("pipeline_onexit.yaml", &run_bin(BIN_ONEXIT, &[]));
}

/// `a.field` lowering: pins the `{{=toJSON(fromJSON(...)['id'])}}`
/// expr-templating + the DAG dep on the producing task.
#[test]
fn emit_pipeline_fields() {
    assert_golden("pipeline_fields.yaml", &run_bin(BIN_FIELDS, &[]));
}

/// `.fan_out`: pins the `withParam` over the list + `{{item}}` arg +
/// the DAG dep on the producer + the aggregated `Vec` consumed after.
#[test]
fn emit_pipeline_fanout() {
    assert_golden("pipeline_fanout.yaml", &run_bin(BIN_FANOUT, &[]));
}

/// `if`/`else`/`else if` lowering: synthesized `when`-gated wrapper
/// workflows + a value-`if` whose `outputs.parameters.return` selects the
/// taken branch via a status-ternary `valueFrom.expression`.
#[test]
fn emit_pipeline_if() {
    assert_golden("pipeline_if.yaml", &run_bin(BIN_IF, &[]));
}

/// Nested-call lowering: a template call in argument position
/// (recursive → output ref + dep) and a call hoisted out of an `if`
/// condition to a parent task.
#[test]
fn emit_pipeline_nested() {
    assert_golden("pipeline_nested.yaml", &run_bin(BIN_NESTED, &[]));
}

/// Attribute param injection: a struct field lowered into `image`
/// (`{{=fromJSON(inputs.parameters['m'])['id']}}`) + (via `pipeline`)
/// `combine`'s injected image/service_account/node_selector.
#[test]
fn emit_pipeline_inject() {
    assert_golden("pipeline_inject.yaml", &run_bin(BIN_INJECT, &[]));
}

/// `#[workflow(node_selector=…)]`: literal key+value set on the dag
/// template (Argo cascades it to every `templateRef`'d task pod),
/// including the raw `{{workflow.parameters.region}}` escape-hatch
/// literal. No `"lit" + arg` injection — workflows are DAGs, not pods.
#[test]
fn emit_pipeline_ns() {
    assert_golden("pipeline_ns.yaml", &run_bin(BIN_NS, &[]));
}

/// `CARGO_ATHENA_DESCRIBE`: the binary reports a `ContainerRunMeta`
/// for one `#[container]`, derived from the *same* `Template::build()`
/// that `emit` uses — the zero-drift contract `cargo athena container
/// emulate` consumes (image, params→env, the binary/host!/artifact
/// ports, the scratch dir, the result path).
#[test]
fn describe_fetch() {
    assert_golden(
        "describe_fetch.json",
        &run_bin(
            BIN_NS,
            &[("CARGO_ATHENA_DESCRIBE", "cargo-athena-example-smoke-fetch")],
        ),
    );
}

/// `CARGO_ATHENA_LIST`: every reachable template's metadata as a JSON
/// array — what `container ls` / `workflow ls` render as a table.
#[test]
fn list_all() {
    assert_golden(
        "list_all.json",
        &run_bin(BIN_NS, &[("CARGO_ATHENA_LIST", "1")]),
    );
}

/// Same, rooted at `pipeline_if` — pins `synthetic: true` on the
/// athena-generated `if`/`else` wrapper + arm sub-workflows (what
/// `workflow ls` hides unless `--include-synthetic`).
#[test]
fn list_if() {
    assert_golden(
        "list_if.json",
        &run_bin(BIN_IF, &[("CARGO_ATHENA_LIST", "1")]),
    );
}

/// Run mode: a container that returns a value (JSON to stdout).
#[test]
fn run_mode_transform() {
    let out = run_bin(
        BIN_PIPELINE,
        &[
            (
                "CARGO_ATHENA_TEMPLATE",
                "cargo-athena-example-smoke-transform",
            ),
            ("CARGO_ATHENA_INPUT", r#"{"data":"hello","factor":4}"#),
        ],
    );
    assert_golden("run_transform.txt", &out);
}

/// Run mode: a container whose real body branches and uses `host!`.
#[test]
fn run_mode_branchy() {
    let out = run_bin(
        BIN_PIPELINE,
        &[
            (
                "CARGO_ATHENA_TEMPLATE",
                "cargo-athena-example-smoke-branchy",
            ),
            ("CARGO_ATHENA_INPUT", r#"{"mode":"fast"}"#),
        ],
    );
    assert_golden("run_branchy.txt", &out);
}
