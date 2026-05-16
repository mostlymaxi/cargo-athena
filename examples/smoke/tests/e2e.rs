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

/// Emit mode: the multi-doc WorkflowTemplate stream for `pipeline`.
#[test]
fn emit_pipeline() {
    assert_golden("pipeline.yaml", &run_bin(BIN_PIPELINE, &[]));
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

/// `#[workflow(on_exit=…)]` -> runnable Workflow `spec.onExit`, plus an
/// `.on_exit(record("done"))` exit hook carrying arguments.
#[test]
fn emit_pipeline_onexit() {
    assert_golden("pipeline_onexit.yaml", &run_bin(BIN_ONEXIT, &[]));
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
            ("CARGO_ATHENA_TEMPLATE", "cargo-athena-example-smoke-branchy"),
            ("CARGO_ATHENA_INPUT", r#"{"mode":"fast"}"#),
        ],
    );
    assert_golden("run_branchy.txt", &out);
}
