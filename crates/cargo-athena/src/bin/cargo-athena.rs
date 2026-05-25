//! `cargo athena` — drive a user crate's cargo-athena binary and
//! cross-compile/package its artifact. Shipped by the `cargo-athena`
//! crate's default `cli` feature, so `cargo install cargo-athena` gives
//! you the `cargo athena` subcommand.
//!
//! The entrypoint is fixed in the user binary's `main`
//! (`cargo_athena::entrypoint!(Root)`); the artifact repository +
//! target matrix come from `athena.toml`.

use cargo_athena::{AthenaConfig, S3Ref, serde_json};
use clap::{Parser, Subcommand};
use std::process::{Command, Stdio, exit};

// Lives in `src/` (not `src/bin/`, which would make it a second
// binary); a `#[path]` module so it stays bin-private and can use the
// helpers below (`cargo_run`, `tool_ok`, `package_meta`, …).
#[path = "../doctor.rs"]
mod doctor;
#[path = "../emulate.rs"]
mod emulate;
#[path = "../feedback.rs"]
mod feedback;
#[path = "../init.rs"]
mod init;
#[path = "../pkg.rs"]
mod pkg;
#[path = "../submit.rs"]
mod submit;
#[path = "../tarball.rs"]
mod tarball;

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
    /// Scaffold a new workflow crate (Cargo.toml, src/main.rs,
    /// athena.toml). Interactive on a TTY; flag-driven otherwise.
    Init(init::InitArgs),
    /// Preflight every prereq for `publish` / `submit` (cargo-zigbuild,
    /// zig, rustup targets, athena.toml, AWS creds, optional S3 reach).
    /// Reports each as green/red with a fix hint. Exit 0 on all-pass.
    Doctor(doctor::DoctorArgs),
    /// Run the user binary in emit-mode; relay the WorkflowTemplate YAML.
    Emit {
        #[command(flatten)]
        pkg: pkg::PkgSel,
        /// Write the YAML here instead of stdout.
        #[arg(long)]
        out: Option<String>,
        /// Also append a convenience runnable `Workflow` (generateName)
        /// for `kubectl create -f -`. Default: templates only — register
        /// them and run with `argo submit --from workflowtemplate/<root>`.
        #[arg(long)]
        with_workflow: bool,
    },
    /// Single-`#[container]` operations (room for more later).
    Container {
        #[command(subcommand)]
        cmd: ContainerCmd,
    },
    /// `#[workflow]` operations.
    Workflow {
        #[command(subcommand)]
        cmd: WorkflowCmd,
    },
    /// Submit a `#[workflow]`/`#[container]` to a cluster: type-checks
    /// args, confirms the binary is uploaded, registers/drift-checks the
    /// `WorkflowTemplate`s (y/N), then creates the run and prints its
    /// name. Talks to the Argo Server (`--argo-server`/`$ARGO_SERVER`)
    /// or the kube API (kubeconfig/in-cluster).
    Submit(submit::SubmitArgs),
    /// Cross-compile + package the tarball locally (no upload); print
    /// the upload key. Use `publish` to build **and** upload in one step.
    Build {
        #[command(flatten)]
        pkg: pkg::PkgSel,
        /// Override the `athena.toml` target matrix (repeatable).
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Dry run: resolve + report the key without building/uploading.
        #[arg(long)]
        print: bool,
    },
    /// One-shot `build` + S3 upload: cross-compile, package, and upload
    /// the tarball to the `athena.toml` artifact repository.
    Publish {
        #[command(flatten)]
        pkg: pkg::PkgSel,
        /// Override the `athena.toml` target matrix (repeatable).
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Upload this prebuilt tarball verbatim instead of building
        /// (build-once / upload-many, e.g. a CI artifact).
        #[arg(long)]
        tarball: Option<String>,
        /// Dry run: resolve + report the key without building/uploading.
        #[arg(long)]
        print: bool,
    },
}

#[derive(Subcommand)]
enum ContainerCmd {
    /// Emulate one `#[container]` locally under docker/podman, exactly
    /// as Argo would: same image, the injected bootstrap,
    /// `ATHENA_PARAM_*` env, the `/athena` scratch dir, `host!` binds,
    /// and S3 artifact ports. By default the binary is *pulled* from
    /// the deployed S3 tarball, so you can smoke-test what's live with
    /// no source on the node.
    Emulate(emulate::EmulateArgs),
    /// Print the runner metadata one template reports — image,
    /// parameters + their types, the binary/`host!`/artifact ports, the
    /// scratch + result paths. Exactly what `emulate` consumes (derived
    /// from the same `Template::build()` as `emit`).
    Describe(emulate::DescribeArgs),
    /// List the templates a workflow binary reports (names + args), so
    /// they're discoverable for `emulate`/`describe`. `--all` includes
    /// `#[workflow]`s + synthetics; default is `#[container]`s only.
    Ls(emulate::LsArgs),
}

#[derive(Subcommand)]
enum WorkflowCmd {
    /// List the `#[workflow]`s in the package (name + typed args).
    /// Synthetic `if`/`else` wrappers + arms are hidden unless
    /// `--include-synthetic`.
    Ls(emulate::WorkflowLsArgs),
    /// Print one workflow's metadata (same as `container describe`,
    /// for any template).
    Describe(emulate::DescribeArgs),
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
        Cmd::Init(args) => init::init(args),
        Cmd::Doctor(args) => doctor::doctor(args),
        Cmd::Emit {
            pkg,
            out,
            with_workflow,
        } => {
            let (package, bin) = pkg.resolve();
            emit(
                package.as_deref(),
                bin.as_deref(),
                out.as_deref(),
                with_workflow,
            );
        }
        Cmd::Container { cmd } => match cmd {
            ContainerCmd::Emulate(args) => emulate::container_emulate(args),
            ContainerCmd::Describe(args) => emulate::describe_print(args),
            ContainerCmd::Ls(args) => emulate::container_ls(args),
        },
        Cmd::Workflow { cmd } => match cmd {
            WorkflowCmd::Ls(args) => emulate::workflow_ls(args),
            WorkflowCmd::Describe(args) => emulate::describe_print(args),
        },
        Cmd::Submit(args) => submit::submit(args),
        Cmd::Build {
            pkg,
            targets,
            print,
        } => {
            let (package, bin) = pkg.resolve();
            build(package.as_deref(), bin.as_deref(), &targets, print);
        }
        Cmd::Publish {
            pkg,
            targets,
            tarball,
            print,
        } => {
            let (package, bin) = pkg.resolve();
            publish(
                package.as_deref(),
                bin.as_deref(),
                &targets,
                tarball.as_deref(),
                print,
            );
        }
    }
}

/// `cargo run` invocation for the user binary. NO `--quiet`: cargo's
/// "Compiling foo..." progress and any compile errors stream to the
/// user's terminal (stderr) by default. Callers that need to capture
/// the binary's stdout (the YAML / JSON payload) should explicitly
/// `.stdout(Stdio::piped())` and let stderr inherit.
fn cargo_run(package: Option<&str>, bin: Option<&str>) -> Command {
    let mut c = Command::new("cargo");
    c.arg("run");
    if let Some(p) = package {
        c.args(["--package", p]);
    }
    if let Some(b) = bin {
        c.args(["--bin", b]);
    }
    c
}

// ---- emit -----------------------------------------------------------------

fn emit(package: Option<&str>, bin: Option<&str>, out: Option<&str>, with_workflow: bool) {
    let mut cmd = cargo_run(package, bin);
    if with_workflow {
        cmd.env("CARGO_ATHENA_WITH_WORKFLOW", "1");
    }
    // stdout = the YAML we want to capture; stderr = cargo's
    // "Compiling..." progress, streams to the user.
    cmd.stdout(Stdio::piped()).stderr(Stdio::inherit());
    let o = cmd.output().expect("failed to run user binary");
    if !o.status.success() {
        exit(o.status.code().unwrap_or(1));
    }
    match out {
        Some(path) => {
            std::fs::write(path, &o.stdout).expect("write --out file");
            eprintln!("wrote {path}");
        }
        None => std::io::Write::write_all(&mut std::io::stdout(), &o.stdout).expect("write stdout"),
    }
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
        msg.push_str("  - cargo-zigbuild  ->  cargo install cargo-zigbuild\n");
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

fn build(package: Option<&str>, bin: Option<&str>, cli_targets: &[String], print: bool) {
    if let Some((tarball, _s3, dest)) = build_tarball(package, bin, cli_targets, print) {
        eprintln!("packaged {tarball}  ->  {dest}");
        eprintln!(
            "(`build` packages only — `cargo athena publish` does \
             cross-compile + package + upload in one step.)"
        );
    }
}

/// The artifact's S3 location (the exact key `emit` injects into every
/// container, so the upload lands where the in-pod bootstrap reads it)
/// plus a human-readable `dest` string. The key is hardcoded as
/// `{crate}/{version}/{bin}.tar.gz`, the same form `BuildCtx::collect`
/// builds in-binary, so the two sites can never drift.
///
/// `AWS_ENDPOINT_URL` can override the endpoint at upload time without
/// changing what `emit` injects.
fn artifact_s3(cfg: &AthenaConfig, krate: &str, version: &str, bin: &str) -> (S3Ref, String) {
    let key = format!("{krate}/{version}/{bin}.tar.gz");
    let repo = &cfg.artifact_repository.s3;
    // Same field mapping core uses to emit the binary artifact.
    let s3 = S3Ref {
        endpoint: repo.endpoint.clone(),
        bucket: repo.bucket.clone(),
        region: repo.region.clone(),
        insecure: repo.insecure,
        key: key.clone(),
    };
    let dest = format!("s3://{}/{} (endpoint {})", s3.bucket, key, s3.endpoint);
    (s3, dest)
}

fn do_upload(s3: &S3Ref, path: &std::path::Path, dest: &str) {
    let st = feedback::step(format!("Uploading {} -> {dest}", path.display()));
    emulate::s3_put(s3, path);
    st.finish();
    // Scriptable: the destination on stdout (all else on stderr).
    println!("s3://{}/{}", s3.bucket, s3.key);
}

/// Resolve the key + print the plan, then (unless `print`)
/// cross-compile every target and package one tarball. Returns
/// `(tarball_path, S3Ref, dest)` for the caller to upload, or `None` on
/// a `--print` dry run. Shared by `build` (package only) and `publish`
/// (build + upload) so the two can never drift.
fn build_tarball(
    package: Option<&str>,
    bin: Option<&str>,
    cli_targets: &[String],
    print: bool,
) -> Option<(String, S3Ref, String)> {
    let cfg = AthenaConfig::load();
    let (krate, version, default_bin) = package_meta(package);
    let bin = bin.map(str::to_string).unwrap_or(default_bin);

    let targets: Vec<String> = if cli_targets.is_empty() {
        cfg.bootstrap.targets.clone()
    } else {
        cli_targets.to_vec()
    };

    let (s3, dest) = artifact_s3(&cfg, &krate, &version, &bin);
    let tarball = format!("target/athena/{bin}.tar.gz");

    eprintln!("crate={krate} version={version} bin={bin}");
    eprintln!("targets: {}", targets.join(", "));
    eprintln!("destination: {dest}");

    if print {
        return None;
    }

    preflight_zig();

    std::fs::create_dir_all("target/athena").expect("mkdir target/athena");
    let stage = std::path::Path::new("target/athena/stage");
    let _ = std::fs::remove_dir_all(stage);
    std::fs::create_dir_all(stage).expect("mkdir stage");

    for t in &targets {
        let st = feedback::step(format!("Cross-compiling for {t}"));
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
            // Drop without finish so the `✗` line marks the failure.
            drop(st);
            exit(status.code().unwrap_or(1));
        }
        let from = format!("target/{t}/release/{bin}");
        let to = stage.join(format!("app-{t}"));
        std::fs::copy(&from, &to)
            .unwrap_or_else(|e| panic!("copy {from} -> {}: {e}", to.display()));
        st.finish();
    }
    let st = feedback::step(format!("Packaging {tarball}"));

    // Pack with pure-Rust `tar`+`flate2` (no host `tar`) under a
    // single top-level `bin/` subdir — see `tarball.rs` for why
    // (Argo's executor `unpack` renames a single top-level entry to
    // the destination path; wrapping in a subdir keeps `/athena/bin`
    // a directory for BOTH single- and multi-arch tarballs).
    let entries: Vec<(std::path::PathBuf, String)> = targets
        .iter()
        .map(|t| (stage.join(format!("app-{t}")), format!("app-{t}")))
        .collect();
    let refs: Vec<(&std::path::Path, &str)> = entries
        .iter()
        .map(|(p, n)| (p.as_path(), n.as_str()))
        .collect();
    if let Err(e) = tarball::create(std::path::Path::new(&tarball), &refs) {
        drop(st);
        eprintln!("tarball create failed: {e}");
        exit(1);
    }
    st.finish();
    Some((tarball, s3, dest))
}

// ---- publish (build + upload, or --tarball: upload prebuilt) --------------

/// Default: `build_tarball` (cross-compile + package) then upload.
/// `--tarball F`: skip the build and upload `F` verbatim (build-once /
/// upload-many — a CI artifact, or the kind e2e dogfood). Upload goes
/// through the shared `emulate::s3_put` (`object_store`, `AWS_*` creds;
/// `AWS_ENDPOINT_URL` overrides the endpoint) — same path as
/// `submit`/`emulate`.
fn publish(
    package: Option<&str>,
    bin: Option<&str>,
    cli_targets: &[String],
    tarball_in: Option<&str>,
    print: bool,
) {
    if let Some(path) = tarball_in {
        let cfg = AthenaConfig::load();
        let (krate, version, default_bin) = package_meta(package);
        let bin = bin.map(str::to_string).unwrap_or(default_bin);
        let (s3, dest) = artifact_s3(&cfg, &krate, &version, &bin);
        let p = std::path::Path::new(path);
        if !p.exists() {
            eprintln!("no tarball at {path}");
            exit(1);
        }
        eprintln!("crate={krate} version={version} bin={bin}");
        eprintln!("upload key: {}", s3.key);
        eprintln!("destination: {dest}");
        if print {
            eprintln!("(--print) would upload {path}");
            return;
        }
        do_upload(&s3, p, &dest);
        return;
    }
    let Some((tarball, s3, dest)) = build_tarball(package, bin, cli_targets, print) else {
        return; // --print dry run: nothing built, nothing to upload
    };
    do_upload(&s3, std::path::Path::new(&tarball), &dest);
}
