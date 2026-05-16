//! Cross-module + cross-crate: the `importing` binary must emit the
//! upstream `cargo-athena-example-smoke-*` templates AND the local
//! cross-module templates (proving the wormhole force-links across both
//! the module and crate boundaries), and must be able to *run* an
//! upstream container.
//!
//!   UPDATE_EXPECT=1 cargo test -p cargo-athena-example-importing

use std::path::PathBuf;
use std::process::Command;

const BIN: &str = env!("CARGO_BIN_EXE_importing");

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden")
        .join(name)
}

fn assert_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_EXPECT").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        eprintln!("updated golden {}", path.display());
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!("missing golden {} — run `UPDATE_EXPECT=1`", path.display())
    });
    assert_eq!(actual, expected, "\n{name} drifted; refresh with UPDATE_EXPECT=1\n");
}

fn run_bin(envs: &[(&str, &str)]) -> String {
    let mut cmd = Command::new(BIN);
    for (k, v) in envs {
        cmd.env(k, v);
    }
    let out = cmd.output().expect("failed to spawn importing binary");
    assert!(
        out.status.success(),
        "importing exited with {:?}\nstderr:\n{}",
        out.status.code(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).expect("stdout was not UTF-8")
}

/// The composed stream must contain the local templates, the local
/// cross-*module* workflow, AND every upstream cross-*crate*
/// `cargo-athena-example-smoke-*` template, all by reference.
#[test]
fn emit_importing_pipeline() {
    let yaml = run_bin(&[]);
    assert!(
        yaml.contains("cargo-athena-example-smoke-pipeline") // cross-crate
            && yaml.contains("cargo-athena-example-smoke-fetch")
            && yaml.contains("cargo-athena-example-importing-module-pipeline") // cross-module
            && yaml.contains("cargo-athena-example-importing-importing-pipeline"),
        "cross-module/cross-crate closure incomplete — wormhole leaked:\n{yaml}"
    );
    assert_golden("importing_pipeline.yaml", &yaml);
}

/// Run mode through the importing binary, dispatching an *upstream*
/// container — only possible if the smoke crate's impl was force-linked.
#[test]
fn run_upstream_container_via_importing() {
    let out = run_bin(&[
        (
            "CARGO_ATHENA_TEMPLATE",
            "cargo-athena-example-smoke-transform",
        ),
        ("CARGO_ATHENA_INPUT", r#"{"data":"x","factor":2}"#),
    ]);
    assert_golden("run_transform_via_importing.txt", &out);
}
