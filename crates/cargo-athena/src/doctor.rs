//! `cargo athena doctor` - preflight every prereq for publishing and
//! submitting workflows. Reports each check as a green / red line with
//! a fix hint when something is missing.
//!
//! Exit code: 0 on all-pass, 1 if any check failed.

use cargo_athena::AthenaConfig;
use std::process::{Command, Stdio, exit};

#[derive(clap::Args)]
pub struct DoctorArgs {
    /// Also try a live HEAD against the configured S3 bucket. Off by
    /// default because it needs working credentials and network.
    #[arg(long)]
    check_s3: bool,
}

const W: usize = 36;
const OK: &str = "\u{2713}";
const X: &str = "\u{2717}";
const Q: &str = "?";

pub fn doctor(args: DoctorArgs) {
    let mut passed = 0usize;
    let mut failed = 0usize;
    let mut warned = 0usize;

    eprintln!();
    eprintln!("cargo athena doctor");
    eprintln!();

    // ---- athena.toml ------------------------------------------------------
    let cfg = match try_load_config() {
        Some((path, cfg)) => {
            check_pass("athena.toml", &format!("loaded {path}"));
            passed += 1;
            Some(cfg)
        }
        None => {
            check_fail(
                "athena.toml",
                "not found",
                Some("create one with `cargo athena init`, or copy from docs/configuration"),
            );
            failed += 1;
            None
        }
    };

    // ---- toolchain --------------------------------------------------------
    match exec_version("cargo-zigbuild", &["--version"]) {
        Some(v) => {
            check_pass("cargo-zigbuild", &v);
            passed += 1;
        }
        None => {
            check_fail(
                "cargo-zigbuild",
                "not found",
                Some("cargo install cargo-zigbuild"),
            );
            failed += 1;
        }
    }
    match exec_version("zig", &["version"]) {
        Some(v) => {
            check_pass("zig", &format!("zig {v}"));
            passed += 1;
        }
        None => {
            check_fail(
                "zig",
                "not found",
                Some("pip install ziglang  (or: brew install zig, or ziglang.org/download)"),
            );
            failed += 1;
        }
    }

    // ---- rustup targets ---------------------------------------------------
    if let Some(cfg) = cfg.as_ref() {
        match rustup_installed_targets() {
            Some(installed) => {
                let mut missing: Vec<&str> = Vec::new();
                for t in &cfg.bootstrap.targets {
                    if installed.iter().any(|i| i == t) {
                        check_pass("rustup target", t);
                        passed += 1;
                    } else {
                        check_fail(
                            "rustup target",
                            &format!("{t} not installed"),
                            Some(&format!("rustup target add {t}")),
                        );
                        failed += 1;
                        missing.push(t.as_str());
                    }
                }
                if cfg.bootstrap.targets.is_empty() {
                    check_warn("rustup target", "athena.toml [bootstrap].targets is empty");
                    warned += 1;
                }
                let _ = missing;
            }
            None => {
                check_warn(
                    "rustup",
                    "couldn't run `rustup target list` (rustup not installed?)",
                );
                warned += 1;
            }
        }
    }

    // ---- AWS credentials --------------------------------------------------
    let aws_env = std::env::var("AWS_ACCESS_KEY_ID").is_ok()
        && std::env::var("AWS_SECRET_ACCESS_KEY").is_ok();
    if aws_env {
        check_pass("AWS credentials", "AWS_ACCESS_KEY_ID set");
        passed += 1;
    } else {
        check_warn(
            "AWS credentials",
            "AWS_ACCESS_KEY_ID / AWS_SECRET_ACCESS_KEY not set (will rely on ambient identity: IMDS / ECS / IRSA)",
        );
        warned += 1;
    }

    // ---- live S3 (opt-in) -------------------------------------------------
    if args.check_s3 {
        if let Some(cfg) = cfg.as_ref() {
            match check_s3_reachable(cfg) {
                Ok(()) => {
                    check_pass(
                        "S3 reachable",
                        &format!(
                            "{} ({})",
                            cfg.artifact_repository.s3.bucket, cfg.artifact_repository.s3.endpoint
                        ),
                    );
                    passed += 1;
                }
                Err(e) => {
                    check_fail(
                        "S3 reachable",
                        &e,
                        Some(
                            "verify athena.toml endpoint + AWS_* env (or AWS_ENDPOINT_URL for port-forward)",
                        ),
                    );
                    failed += 1;
                }
            }
        } else {
            check_warn("S3 reachable", "skipped (no athena.toml)");
            warned += 1;
        }
    }

    // ---- summary ----------------------------------------------------------
    eprintln!();
    let total = passed + failed + warned;
    if failed == 0 && warned == 0 {
        eprintln!("All {total} checks passed.");
        exit(0);
    } else if failed == 0 {
        eprintln!("{passed} of {total} passed, {warned} warning(s) - okay, but read above.");
        exit(0);
    } else {
        eprintln!(
            "{passed} of {total} passed, {failed} failed{w} - fix the above to publish.",
            w = if warned > 0 {
                format!(", {warned} warning(s)")
            } else {
                String::new()
            }
        );
        exit(1);
    }
}

// ---- check primitives -----------------------------------------------------

fn check_pass(name: &str, detail: &str) {
    eprintln!("  {name:.<W$} {OK} {detail}");
}

fn check_fail(name: &str, msg: &str, fix: Option<&str>) {
    eprintln!("  {name:.<W$} {X} {msg}");
    if let Some(fix) = fix {
        eprintln!("  {:>W$}      fix: {fix}", "");
    }
}

fn check_warn(name: &str, msg: &str) {
    eprintln!("  {name:.<W$} {Q} {msg}");
}

// ---- implementations ------------------------------------------------------

fn try_load_config() -> Option<(String, AthenaConfig)> {
    // `main()` has already resolved the effective config (`--config`,
    // `$ATHENA_CONFIG`, the repo-local `./athena.toml`, or the global
    // `~/.config` fallback) and exported `ATHENA_CONFIG`. Report whatever
    // it landed on; the path itself shows which source won (e.g. a
    // `~/.config/...` path means the global fallback). `is_file` guards
    // against `AthenaConfig::load`'s panic on a missing file.
    let path = std::env::var_os("ATHENA_CONFIG").map(std::path::PathBuf::from)?;
    if !path.is_file() {
        return None;
    }
    let cfg = AthenaConfig::load();
    Some((path.display().to_string(), cfg))
}

fn exec_version(cmd: &str, args: &[&str]) -> Option<String> {
    let out = Command::new(cmd)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8_lossy(&out.stdout).trim().to_string();
    Some(s.lines().next().unwrap_or("").trim().to_string())
}

fn rustup_installed_targets() -> Option<Vec<String>> {
    let out = Command::new("rustup")
        .args(["target", "list", "--installed"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    Some(
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .map(|l| l.trim().to_string())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

fn check_s3_reachable(cfg: &AthenaConfig) -> Result<(), String> {
    // A sentinel key we don't care about; we only need the request to
    // make it to the server. 404 is still "reachable".
    let s3 = cargo_athena::S3Ref::from_repo(
        &cfg.artifact_repository.s3,
        ".athena-doctor-probe".to_string(),
    );
    let store = crate::emulate::s3_store(&s3);
    let key = object_store::path::Path::from(s3.key.as_str());
    let result = crate::emulate::rt()
        .block_on(async { object_store::ObjectStore::head(&store, &key).await });
    match result {
        Ok(_) => Ok(()),
        Err(object_store::Error::NotFound { .. }) => Ok(()), // bucket reachable, key absent
        Err(e) => Err(format!("{e}")),
    }
}
