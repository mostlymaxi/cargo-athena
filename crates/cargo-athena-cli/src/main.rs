//! `cargo athena` — drive a user crate's cargo-athena binary and
//! cross-compile/publish its artifact.
//!
//!   cargo athena emit    [--package P] [--bin B] [--out FILE]
//!       run the user binary in emit-mode, relay the WorkflowTemplate YAML
//!   cargo athena run     --template <argo-name> [--package P] [--bin B] [--input JSON]
//!       run one container's body locally (in-process)
//!   cargo athena build   [--package P] [--bin B] [--target T].. [--print]
//!       cross-compile static-musl binaries for the athena.toml target
//!       matrix, package app-<triple> into one tarball, print the upload key
//!   cargo athena publish [--package P] [--bin B]            (not yet)
//!
//! The entrypoint is fixed in the user binary's `main`; the artifact
//! repository + target matrix come from `athena.toml`.

use cargo_athena_core::{AthenaConfig, serde_json};
use std::process::{Command, exit};

fn main() {
    // `cargo athena <args...>` => argv `cargo-athena athena <args...>`.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("athena") {
        args.remove(0);
    }
    let sub = args.first().cloned().unwrap_or_default();
    let rest = &args[1.min(args.len())..];

    match sub.as_str() {
        "emit" => emit(rest),
        "run" => run(rest),
        "build" => build(rest),
        "publish" => publish(rest),
        _ => {
            eprintln!(
                "usage:\n  \
                 cargo athena emit    [--package P] [--bin B] [--out FILE]\n  \
                 cargo athena run     --template <argo-name> [--package P] [--bin B] [--input JSON]\n  \
                 cargo athena build   [--package P] [--bin B] [--target T].. [--print]\n  \
                 cargo athena publish [--package P] [--bin B]"
            );
            exit(2);
        }
    }
}

fn opt<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

fn opts<'a>(args: &'a [String], name: &str) -> Vec<&'a str> {
    args.iter()
        .enumerate()
        .filter(|(_, a)| a.as_str() == name)
        .filter_map(|(i, _)| args.get(i + 1))
        .map(String::as_str)
        .collect()
}

fn has(args: &[String], flag: &str) -> bool {
    args.iter().any(|a| a == flag)
}

fn cargo_run(args: &[String]) -> Command {
    let mut c = Command::new("cargo");
    c.args(["run", "--quiet"]);
    if let Some(p) = opt(args, "--package") {
        c.args(["--package", p]);
    }
    if let Some(b) = opt(args, "--bin") {
        c.args(["--bin", b]);
    }
    c
}

// ---- emit -----------------------------------------------------------------

fn emit(args: &[String]) {
    let out = cargo_run(args).output().expect("failed to run user binary");
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        exit(out.status.code().unwrap_or(1));
    }
    match opt(args, "--out") {
        Some(path) => {
            std::fs::write(path, &out.stdout).expect("write --out file");
            eprintln!("wrote {path}");
        }
        None => print!("{}", String::from_utf8_lossy(&out.stdout)),
    }
}

// ---- run ------------------------------------------------------------------

fn run(args: &[String]) {
    let template = opt(args, "--template").expect("--template is required");
    let mut cmd = cargo_run(args);
    cmd.env("CARGO_ATHENA_TEMPLATE", template);
    if let Some(input) = opt(args, "--input") {
        cmd.env("CARGO_ATHENA_INPUT", input);
    }
    let status = cmd.status().expect("failed to run user binary");
    exit(status.code().unwrap_or(1));
}

// ---- build (cross-compile) ------------------------------------------------

/// Resolve (package, version, bin) from `cargo metadata`.
fn package_meta(pkg: Option<&str>) -> (String, String, String) {
    let out = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--no-deps"])
        .output()
        .expect("cargo metadata failed");
    let meta: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("parse cargo metadata");
    let packages = meta["packages"].as_array().cloned().unwrap_or_default();
    let p = match pkg {
        Some(name) => packages
            .iter()
            .find(|p| p["name"] == serde_json::json!(name))
            .unwrap_or_else(|| panic!("package {name:?} not found")),
        None if packages.len() == 1 => &packages[0],
        None => panic!("multiple packages; pass --package"),
    };
    let name = p["name"].as_str().unwrap().to_string();
    let version = p["version"].as_str().unwrap().to_string();
    // (crate, version, default bin = crate name)
    (name.clone(), version, name)
}

fn render_key(template: &str, krate: &str, version: &str, bin: &str) -> String {
    template
        .replace("{crate}", krate)
        .replace("{version}", version)
        .replace("{bin}", bin)
}

fn build(args: &[String]) {
    let cfg = AthenaConfig::load();
    let (krate, version, default_bin) = package_meta(opt(args, "--package"));
    let bin = opt(args, "--bin").map(str::to_string).unwrap_or(default_bin);

    let targets: Vec<String> = {
        let cli = opts(args, "--target");
        if cli.is_empty() {
            cfg.bootstrap.targets.clone()
        } else {
            cli.into_iter().map(str::to_string).collect()
        }
    };

    let key = render_key(&cfg.artifact.key, &krate, &version, &bin);
    let s3 = &cfg.artifact_repository.s3;
    let dest = format!("s3://{}/{} (endpoint {})", s3.bucket, key, s3.endpoint);
    let tarball = format!("target/athena/{bin}.tar.gz");

    eprintln!("crate={krate} version={version} bin={bin}");
    eprintln!("targets: {}", targets.join(", "));
    for t in &targets {
        eprintln!("  cargo zigbuild --release --target {t} -p {krate} --bin {bin}  ->  app-{t}");
    }
    eprintln!("tarball: {tarball}");
    eprintln!("upload key: {key}");
    eprintln!("destination: {dest}");

    if has(args, "--print") {
        return;
    }

    std::fs::create_dir_all("target/athena").expect("mkdir target/athena");
    let stage = std::path::Path::new("target/athena/stage");
    let _ = std::fs::remove_dir_all(stage);
    std::fs::create_dir_all(stage).expect("mkdir stage");

    for t in &targets {
        let status = Command::new("cargo")
            .args([
                "zigbuild",
                "--release",
                "--target",
                t,
                "-p",
                &krate,
                "--bin",
                &bin,
            ])
            .status()
            .expect("cargo zigbuild failed to start");
        if !status.success() {
            eprintln!("zigbuild failed for {t}");
            exit(status.code().unwrap_or(1));
        }
        let from = format!("target/{t}/release/{bin}");
        let to = stage.join(format!("app-{t}"));
        std::fs::copy(&from, &to)
            .unwrap_or_else(|e| panic!("copy {from} -> {}: {e}", to.display()));
    }

    let status = Command::new("tar")
        .args(["-czf", &tarball, "-C"])
        .arg(stage)
        .arg(".")
        .status()
        .expect("tar failed to start");
    if !status.success() {
        exit(status.code().unwrap_or(1));
    }
    eprintln!("packaged {tarball}  ->  {dest}");
    eprintln!("(`cargo athena publish` to upload — not yet implemented)");
}

// ---- publish (stub) -------------------------------------------------------

fn publish(args: &[String]) {
    let cfg = AthenaConfig::load();
    let (krate, version, default_bin) = package_meta(opt(args, "--package"));
    let bin = opt(args, "--bin").map(str::to_string).unwrap_or(default_bin);
    let key = render_key(&cfg.artifact.key, &krate, &version, &bin);
    let s3 = &cfg.artifact_repository.s3;
    eprintln!(
        "publish is not implemented yet.\n\
         would upload target/athena/{bin}.tar.gz -> s3://{}/{} (endpoint {})",
        s3.bucket, key, s3.endpoint
    );
    exit(2);
}
