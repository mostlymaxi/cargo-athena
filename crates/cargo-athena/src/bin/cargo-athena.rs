//! `cargo athena` — drive a user crate's cargo-athena binary and
//! cross-compile/package its artifact. Shipped by the `cargo-athena`
//! crate's default `cli` feature, so `cargo install cargo-athena` gives
//! you the `cargo athena` subcommand.
//!
//! The entrypoint is fixed in the user binary's `main`
//! (`cargo_athena::entrypoint::<Root>()`); the artifact repository +
//! target matrix come from `athena.toml`.

use cargo_athena::{AthenaConfig, serde_json};
use clap::{Parser, Subcommand};
use std::process::{Command, Stdio, exit};

/// Cargo plugin shim: invoked as `cargo athena <cmd>` → argv
/// `cargo-athena athena <cmd>`, so `athena` is the wrapper subcommand.
#[derive(Parser)]
#[command(bin_name = "cargo")]
enum Cargo {
    /// Compile regular Rust into Argo Workflow YAML.
    Athena(Athena),
}

#[derive(clap::Args)]
#[command(version, about, long_about = None)]
struct Athena {
    /// Path to `athena.toml`. Default: the nearest one found walking up
    /// from the cwd (like `Cargo.toml`), or `$ATHENA_CONFIG`.
    #[arg(short = 'c', long = "config", global = true, value_name = "FILE")]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the user binary in emit-mode; relay the WorkflowTemplate YAML.
    Emit {
        #[arg(long)]
        package: Option<String>,
        #[arg(long)]
        bin: Option<String>,
        /// Write the YAML here instead of stdout.
        #[arg(long)]
        out: Option<String>,
        /// Also append a convenience runnable `Workflow` (generateName)
        /// for `kubectl create -f -`. Default: templates only — register
        /// them and run with `argo submit --from workflowtemplate/<root>`.
        #[arg(long)]
        with_workflow: bool,
    },
    /// Run one container's body locally, in-process.
    Run {
        /// Argo template name (`<crate>-<fn>` kebab, or the
        /// `#[container(name = "…")]` override).
        #[arg(long)]
        template: String,
        #[arg(long)]
        package: Option<String>,
        #[arg(long)]
        bin: Option<String>,
        /// JSON object of the function's arguments.
        #[arg(long)]
        input: Option<String>,
    },
    /// Cross-compile static-musl binaries, package the tarball, print
    /// the upload key.
    Build {
        #[arg(long)]
        package: Option<String>,
        #[arg(long)]
        bin: Option<String>,
        /// Override the `athena.toml` target matrix (repeatable).
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Dry run: resolve + report the key without building/uploading.
        #[arg(long)]
        print: bool,
    },
    /// (not yet implemented) upload the packaged tarball.
    Publish {
        #[arg(long)]
        package: Option<String>,
        #[arg(long)]
        bin: Option<String>,
    },
}

fn main() {
    let Cargo::Athena(a) = Cargo::parse();
    if let Some(cfg) = &a.config {
        let abs = std::fs::canonicalize(cfg).unwrap_or_else(|e| {
            eprintln!("--config {}: {e}", cfg.display());
            exit(2);
        });
        // One unified mechanism: `AthenaConfig::load()` (in core) reads
        // `ATHENA_CONFIG`, and the `cargo run` child we spawn for
        // emit/run inherits it. SAFETY: single-threaded, set before any
        // thread or child process exists.
        unsafe { std::env::set_var("ATHENA_CONFIG", &abs) };
    }
    match a.cmd {
        Cmd::Emit {
            package,
            bin,
            out,
            with_workflow,
        } => emit(
            package.as_deref(),
            bin.as_deref(),
            out.as_deref(),
            with_workflow,
        ),
        Cmd::Run {
            template,
            package,
            bin,
            input,
        } => run(
            &template,
            package.as_deref(),
            bin.as_deref(),
            input.as_deref(),
        ),
        Cmd::Build {
            package,
            bin,
            targets,
            print,
        } => build(package.as_deref(), bin.as_deref(), &targets, print),
        Cmd::Publish { package, bin } => {
            publish(package.as_deref(), bin.as_deref())
        }
    }
}

fn cargo_run(package: Option<&str>, bin: Option<&str>) -> Command {
    let mut c = Command::new("cargo");
    c.args(["run", "--quiet"]);
    if let Some(p) = package {
        c.args(["--package", p]);
    }
    if let Some(b) = bin {
        c.args(["--bin", b]);
    }
    c
}

// ---- emit -----------------------------------------------------------------

fn emit(
    package: Option<&str>,
    bin: Option<&str>,
    out: Option<&str>,
    with_workflow: bool,
) {
    let mut cmd = cargo_run(package, bin);
    if with_workflow {
        cmd.env("CARGO_ATHENA_WITH_WORKFLOW", "1");
    }
    let o = cmd.output().expect("failed to run user binary");
    if !o.status.success() {
        eprint!("{}", String::from_utf8_lossy(&o.stderr));
        exit(o.status.code().unwrap_or(1));
    }
    match out {
        Some(path) => {
            std::fs::write(path, &o.stdout).expect("write --out file");
            eprintln!("wrote {path}");
        }
        None => print!("{}", String::from_utf8_lossy(&o.stdout)),
    }
}

// ---- run ------------------------------------------------------------------

fn run(
    template: &str,
    package: Option<&str>,
    bin: Option<&str>,
    input: Option<&str>,
) {
    let mut cmd = cargo_run(package, bin);
    cmd.env("CARGO_ATHENA_TEMPLATE", template);
    if let Some(i) = input {
        cmd.env("CARGO_ATHENA_INPUT", i);
    }
    let status = cmd.status().expect("failed to run user binary");
    exit(status.code().unwrap_or(1));
}

// ---- build (cross-compile) ------------------------------------------------

/// Resolve `(crate, version, default_bin)` from `cargo metadata`.
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
    (name.clone(), version, name)
}

fn render_key(template: &str, krate: &str, version: &str, bin: &str) -> String {
    template
        .replace("{crate}", krate)
        .replace("{version}", version)
        .replace("{bin}", bin)
}

/// `cmd args…` exits 0 (tool is present + runnable).
fn tool_ok(cmd: &str, args: &[&str]) -> bool {
    Command::new(cmd)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// `build` cross-links with `cargo-zigbuild`, which uses `zig cc` as the
/// linker — so BOTH are required. Fail explicitly with the fix instead
/// of a cryptic mid-link error. (Not called for `--print` dry runs.)
fn preflight_zig() {
    let no_zigbuild = !tool_ok("cargo-zigbuild", &["--version"]);
    let no_zig = !tool_ok("zig", &["version"]);
    if !no_zigbuild && !no_zig {
        return;
    }
    let mut msg = String::from(
        "`cargo athena build` cross-compiles with the Zig toolchain, \
         which is missing:\n",
    );
    if no_zigbuild {
        msg.push_str(
            "  - cargo-zigbuild  ->  cargo install cargo-zigbuild\n",
        );
    }
    if no_zig {
        msg.push_str(
            "  - zig             ->  https://ziglang.org/download/  \
             (or `pip install ziglang`, or your package manager)\n",
        );
    }
    msg.push_str(
        "(the repo's `nix develop` shell provides both. `cargo athena \
         emit` and `--print` need neither.)",
    );
    eprintln!("{msg}");
    exit(1);
}

fn build(
    package: Option<&str>,
    bin: Option<&str>,
    cli_targets: &[String],
    print: bool,
) {
    let cfg = AthenaConfig::load();
    let (krate, version, default_bin) = package_meta(package);
    let bin = bin.map(str::to_string).unwrap_or(default_bin);

    let targets: Vec<String> = if cli_targets.is_empty() {
        cfg.bootstrap.targets.clone()
    } else {
        cli_targets.to_vec()
    };

    let key = render_key(&cfg.artifact.key, &krate, &version, &bin);
    let s3 = &cfg.artifact_repository.s3;
    let dest = format!("s3://{}/{} (endpoint {})", s3.bucket, key, s3.endpoint);
    let tarball = format!("target/athena/{bin}.tar.gz");

    eprintln!("crate={krate} version={version} bin={bin}");
    eprintln!("targets: {}", targets.join(", "));
    for t in &targets {
        eprintln!(
            "  cargo zigbuild --release --target {t} -p {krate} --bin {bin}  ->  app-{t}"
        );
    }
    eprintln!("tarball: {tarball}");
    eprintln!("upload key: {key}");
    eprintln!("destination: {dest}");

    if print {
        return;
    }

    preflight_zig();

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

fn publish(package: Option<&str>, bin: Option<&str>) {
    let cfg = AthenaConfig::load();
    let (krate, version, default_bin) = package_meta(package);
    let bin = bin.map(str::to_string).unwrap_or(default_bin);
    let key = render_key(&cfg.artifact.key, &krate, &version, &bin);
    let s3 = &cfg.artifact_repository.s3;
    eprintln!(
        "publish is not implemented yet.\n\
         would upload target/athena/{bin}.tar.gz -> s3://{}/{} (endpoint {})",
        s3.bucket, key, s3.endpoint
    );
    exit(2);
}
