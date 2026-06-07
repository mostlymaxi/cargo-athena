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
// helpers below (`tool_ok`, `package_meta`, …).
#[path = "../binsrc.rs"]
mod binsrc;
#[path = "../doctor.rs"]
mod doctor;
#[path = "../emulate.rs"]
mod emulate;
#[path = "../feedback.rs"]
mod feedback;
#[path = "../gitinfo.rs"]
mod gitinfo;
#[path = "../init.rs"]
mod init;
#[path = "../ls.rs"]
mod ls;
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
#[command(
    version,
    about,
    long_about = None,
    after_help = "Typical flow:  init -> publish -> submit"
)]
struct Athena {
    /// Path to `athena.toml`.
    ///
    /// Default: the nearest one found walking up from the cwd (like
    /// `Cargo.toml`), or `$ATHENA_CONFIG`.
    #[arg(short = 'c', long = "config", global = true, value_name = "FILE")]
    config: Option<std::path::PathBuf>,
    #[command(subcommand)]
    cmd: Cmd,
}

// Variant order is the rendered help order (clap lists subcommands as
// declared): a lifecycle arc, author -> inspect -> run -> ship -> deploy
// -> diagnose. Each doc is a punchy first line (the one-liner shown in
// the command list) + a blank line + detail (shown under `<cmd> --help`).
#[derive(Subcommand)]
enum Cmd {
    /// Scaffold a new workflow crate
    ///
    /// Writes a `Cargo.toml`, `src/main.rs`, and `athena.toml`.
    /// Interactive on a TTY; flag-driven otherwise.
    Init(init::InitArgs),
    /// List the templates a binary exposes
    ///
    /// Shows both `#[container]`s and `#[workflow]`s. `--kind` filters;
    /// synthetic `if`/`else` internals are hidden unless
    /// `--include-synthetic`.
    Ls(ls::LsArgs),
    /// Show a template's inputs, image, and submit line
    ///
    /// Prints one template's metadata: signature, image, mounts, and a
    /// copy-pasteable submit line. Defaults to the binary's root template;
    /// pick another with `-w/--workflow`.
    Describe(emulate::DescribeArgs),
    /// Print a binary's WorkflowTemplate YAML
    ///
    /// Runs a workflow binary in emit-mode and relays the WorkflowTemplate
    /// YAML to stdout.
    Emit {
        #[command(flatten)]
        bin: binsrc::BinSel,
        /// Build a dev version and name its slot
        ///
        /// Symmetric with `publish --dev-tag`; bare `--dev-tag` = the short
        /// commit. Source build only (not with a positional prebuilt binary).
        #[arg(long = "dev-tag", value_name = "SLOT", num_args = 0..=1)]
        dev_tag: Option<Option<String>>,
        /// Write the YAML here instead of stdout.
        #[arg(long)]
        out: Option<String>,
        /// Also append a convenience runnable `Workflow`
        ///
        /// A `generateName` Workflow for `kubectl create -f -`. Default:
        /// templates only; register them and run with `argo submit --from
        /// workflowtemplate/<root>`.
        #[arg(long)]
        with_workflow: bool,
    },
    /// Run one container locally under Docker/Podman
    ///
    /// Emulates one `#[container]` exactly as Argo would (same image, the
    /// injected bootstrap, positional argv, the `/athena` scratch dir,
    /// `host!` binds, S3 artifact ports). The run payload is the deployed
    /// S3 tarball by default (`--build` / `--tarball` to override).
    Emulate(emulate::EmulateArgs),
    /// Cross-compile and package the binary (no upload)
    ///
    /// Packages the tarball locally and prints the upload key. Use
    /// `publish` to build **and** upload in one step.
    Build {
        #[command(flatten)]
        pkg: pkg::PkgSel,
        /// Override the `athena.toml` target matrix (repeatable).
        #[arg(long = "target")]
        targets: Vec<String>,
        #[command(flatten)]
        gate: GateArgs,
        /// Dry run: resolve + report the key without building/uploading.
        #[arg(long)]
        print: bool,
    },
    /// Build and upload the binary to S3
    ///
    /// One-shot `build` + S3 upload: cross-compile, package, and upload the
    /// tarball to the `athena.toml` artifact repository.
    Publish {
        #[command(flatten)]
        pkg: pkg::PkgSel,
        /// Override the `athena.toml` target matrix (repeatable).
        #[arg(long = "target")]
        targets: Vec<String>,
        /// Upload this prebuilt tarball verbatim instead of building
        ///
        /// Build-once / upload-many, e.g. a CI artifact.
        #[arg(long)]
        tarball: Option<String>,
        #[command(flatten)]
        gate: GateArgs,
        /// Dry run: resolve + report the key without building/uploading.
        #[arg(long)]
        print: bool,
    },
    /// Submit a workflow or container to a cluster
    ///
    /// Type-checks args, confirms the binary is uploaded, registers and
    /// drift-checks the `WorkflowTemplate`s (y/N), then creates the run and
    /// prints its name. Talks to the Argo Server
    /// (`--argo-server`/`$ARGO_SERVER`) or the kube API
    /// (kubeconfig/in-cluster).
    Submit(submit::SubmitArgs),
    /// Delete one deployed version's templates and binary
    ///
    /// Removes the `WorkflowTemplate`s tagged `cargo.athena/tag=<TAG>` plus
    /// the `{pkg}/<TAG>/{bin}.tar.gz` S3 binary (`--keep-binary` spares it).
    /// `<TAG>` is a dev slot (`dev-foo`), a release tag (`0-6-0`), or a raw
    /// semver (`0.6.0`). For cleaning up dev iterations.
    Prune(submit::PruneArgs),
    /// Check publish/submit prerequisites
    ///
    /// Preflights every prereq for `publish` / `submit` (cargo-zigbuild,
    /// zig, rustup targets, athena.toml, AWS creds, optional S3 reach).
    /// Reports each as green/red with a fix hint. Exit 0 on all-pass.
    Doctor(doctor::DoctorArgs),
}

/// The release-gate flags shared by `build` / `publish`. The version tag
/// they resolve is baked into the binary (and used as the S3 key), so a
/// clean build on a release branch is the only path to a `kebab(semver)`
/// release; everything else is a dev version. `--allow-dirty` and the
/// off-branch confirm are deliberately SEPARATE gates (dirty = binary
/// integrity; off-branch = release provenance).
#[derive(clap::Args)]
struct GateArgs {
    /// Build a dev version and name its slot
    ///
    /// Bare `--dev-tag` = the short commit (`dev-<sha>`); `--dev-tag foo` =
    /// `dev-foo` (a stable slot you overwrite while iterating). Forces the
    /// dev channel even on a clean release branch.
    #[arg(long = "dev-tag", value_name = "TAG", num_args = 0..=1)]
    dev_tag: Option<Option<String>>,
    /// Allow a dirty working tree
    ///
    /// Uncommitted changes get baked into the binary. Required when
    /// building off an uncommitted tree (a dev version).
    #[arg(long = "allow-dirty")]
    allow_dirty: bool,
    /// Skip the off-release-branch confirmation prompt (for CI).
    #[arg(long)]
    yes: bool,
}

/// `~/.config/cargo-athena` (honoring `$XDG_CONFIG_HOME`), where a global
/// `athena.toml` may live so the consumer commands (`submit` / `emit` /
/// `ls` / `describe`) work without a per-repo config. `None` if the home
/// dir can't be determined. The repo-local `./athena.toml` always wins
/// over this (see `AthenaConfig::resolve_config_path`). Forced `Xdg`
/// strategy => same path on mac and linux.
fn global_config_dir() -> Option<std::path::PathBuf> {
    use etcetera::base_strategy::{BaseStrategy, Xdg};
    Xdg::new()
        .ok()
        .map(|xdg| xdg.config_dir().join("cargo-athena"))
}

fn main() {
    let Cargo::Athena(a) = Cargo::parse();
    // Resolve the effective `athena.toml` ONCE up front and export it via
    // `ATHENA_CONFIG`, so every in-process helper AND the spawned user
    // binary load the same file. Order (see `AthenaConfig::resolve_config_path`):
    // `--config` -> `$ATHENA_CONFIG` -> `./athena.toml` (walking up, the
    // repo/dev case) -> `~/.config/cargo-athena/athena.toml` (global, for
    // source-free `submit`/`emit`/etc). The global step is the CLI's job
    // (core never reaches for `etcetera`); core's own `load()` just reads
    // the `ATHENA_CONFIG` we export here.
    let env_cfg = std::env::var_os("ATHENA_CONFIG").map(std::path::PathBuf::from);
    let cwd = std::env::current_dir().unwrap_or_default();
    let xdg = global_config_dir();
    if let Some(path) = AthenaConfig::resolve_config_path(
        a.config.as_deref(),
        env_cfg.as_deref(),
        &cwd,
        xdg.as_deref(),
    ) {
        // Absolutize so the spawned binary (run from a different cwd)
        // resolves the same file. An explicit, unreadable `--config` is a
        // hard error (preserve the prior behavior); a discovered or
        // inherited path is taken best-effort.
        let abs = std::fs::canonicalize(&path).unwrap_or_else(|e| {
            if a.config.is_some() {
                eprintln!("--config {}: {e}", path.display());
                exit(2);
            }
            path.clone()
        });
        // SAFETY: single-threaded, set before any thread or child exists.
        unsafe { std::env::set_var("ATHENA_CONFIG", &abs) };
    }
    match a.cmd {
        Cmd::Init(args) => init::init(args),
        Cmd::Doctor(args) => doctor::doctor(args),
        Cmd::Emit {
            bin,
            dev_tag,
            out,
            with_workflow,
        } => {
            bin.apply_dev_tag(dev_tag);
            emit(&bin.resolve(), out.as_deref(), with_workflow);
        }
        Cmd::Ls(args) => ls::ls(args),
        Cmd::Describe(args) => emulate::describe(args),
        Cmd::Emulate(args) => emulate::emulate(args),
        Cmd::Submit(args) => submit::submit(args),
        Cmd::Prune(args) => submit::prune(args),
        Cmd::Build {
            pkg,
            targets,
            gate,
            print,
        } => {
            let (package, bin) = pkg.resolve();
            build(package.as_deref(), bin.as_deref(), &targets, gate, print);
        }
        Cmd::Publish {
            pkg,
            targets,
            tarball,
            gate,
            print,
        } => {
            let (package, bin) = pkg.resolve();
            publish(
                package.as_deref(),
                bin.as_deref(),
                &targets,
                tarball.as_deref(),
                gate,
                print,
            );
        }
    }
}

// ---- emit -----------------------------------------------------------------

fn emit(src: &binsrc::BinarySource, out: Option<&str>, with_workflow: bool) {
    // Confirm it's a cargo-athena binary (and protocol-compatible) before
    // trusting its emit output.
    src.probe();
    let mut cmd = src.command();
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

fn build(
    package: Option<&str>,
    bin: Option<&str>,
    cli_targets: &[String],
    gate: GateArgs,
    print: bool,
) {
    if let Some((tarball, _s3, dest)) = build_tarball(package, bin, cli_targets, gate, print) {
        eprintln!("packaged {tarball}  ->  {dest}");
        eprintln!(
            "(`build` packages only — `cargo athena publish` does \
             cross-compile + package + upload in one step.)"
        );
    }
}

/// The artifact's S3 location (the exact key `emit` injects into every
/// container, so the upload lands where the in-pod bootstrap reads it)
/// plus a human-readable `dest` string. The key is `{crate}/<tag>/{bin}
/// .tar.gz`, the same form `BuildCtx::collect` builds in-binary from the
/// baked `version_tag`, so the upload and the emitted YAML can't drift —
/// and a dev binary never overwrites a release tarball.
///
/// `AWS_ENDPOINT_URL` can override the endpoint at upload time without
/// changing what `emit` injects.
fn artifact_s3(cfg: &AthenaConfig, krate: &str, tag: &str, bin: &str) -> (S3Ref, String) {
    let key = cargo_athena::api::munge::binary_key(krate, tag, bin);
    let s3 = S3Ref::from_repo(&cfg.artifact_repository.s3, key);
    let dest = format!("s3://{}/{} (endpoint {})", s3.bucket, s3.key, s3.endpoint);
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
    gate: GateArgs,
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

    // Resolve the build-time-sealed version tag (git-aware + the two
    // gates; not gated on a `--print` dry run). It keys the S3 upload AND
    // is baked into the binary below, so the two agree by construction.
    let bt = gitinfo::resolve(&version, gate.dev_tag, gate.allow_dirty, gate.yes, !print);
    let (s3, dest) = artifact_s3(&cfg, &krate, &bt.tag, &bin);
    let tarball = format!("target/athena/{bin}.tar.gz");

    eprintln!("crate={krate} version={version} bin={bin}");
    eprintln!("tag={} channel={}", bt.tag, bt.channel);
    eprintln!("targets: {}", targets.join(", "));
    eprintln!("destination: {dest}");

    if print {
        return None;
    }

    // Bake the resolved tag + provenance into the binary: rustc reads
    // these via `option_env!` (in `entrypoint!`) at compile time, and the
    // cargo children below inherit this process's env. Setting them here
    // (not per-Command) means one source for every target build.
    // SAFETY: single-threaded; set before spawning any cargo child.
    unsafe {
        std::env::set_var("ATHENA_VERSION_TAG", &bt.tag);
        if let Some(c) = &bt.commit {
            std::env::set_var("ATHENA_GIT_COMMIT", c);
        }
        if bt.dirty {
            std::env::set_var("ATHENA_GIT_DIRTY", "true");
        }
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
    gate: GateArgs,
    print: bool,
) {
    if let Some(path) = tarball_in {
        let cfg = AthenaConfig::load();
        let (krate, version, default_bin) = package_meta(package);
        let bin = bin.map(str::to_string).unwrap_or(default_bin);
        // A prebuilt tarball already has its tag baked in; resolve the
        // SAME tag here for the upload key. No gating (the build, not the
        // upload, is where the dirty/branch gates belong).
        let dev_tag_given = gate.dev_tag.is_some();
        let bt = gitinfo::resolve(&version, gate.dev_tag, gate.allow_dirty, gate.yes, false);
        // The one case where the resolved tag is a GUESS that can diverge
        // from the tarball's baked tag is a bare dev build: the slot then
        // defaults to the CURRENT commit, which need not match the build's.
        // Refuse rather than upload to a key the binary won't reference.
        // (A release tag, an explicit --dev-tag, or ATHENA_VERSION_TAG are
        // all deterministic and fine.)
        let explicit_tag = std::env::var_os("ATHENA_VERSION_TAG").is_some_and(|v| !v.is_empty());
        if bt.channel == "dev" && !dev_tag_given && !explicit_tag {
            eprintln!(
                "error: `publish --tarball` can't infer the prebuilt binary's \
                 dev tag from the current tree (it would guess `{}` from the \
                 working commit).\n  Pass the tag the tarball was built with: \
                 `--dev-tag <slot>`, or set `ATHENA_VERSION_TAG=<tag>`.",
                bt.tag
            );
            exit(2);
        }
        let (s3, dest) = artifact_s3(&cfg, &krate, &bt.tag, &bin);
        let p = std::path::Path::new(path);
        if !p.exists() {
            eprintln!("no tarball at {path}");
            exit(1);
        }
        eprintln!("crate={krate} version={version} bin={bin}");
        eprintln!("tag={} channel={}", bt.tag, bt.channel);
        eprintln!("upload key: {}", s3.key);
        eprintln!("destination: {dest}");
        if print {
            eprintln!("(--print) would upload {path}");
            return;
        }
        do_upload(&s3, p, &dest);
        return;
    }
    let Some((tarball, s3, dest)) = build_tarball(package, bin, cli_targets, gate, print) else {
        return; // --print dry run: nothing built, nothing to upload
    };
    do_upload(&s3, std::path::Path::new(&tarball), &dest);
}
