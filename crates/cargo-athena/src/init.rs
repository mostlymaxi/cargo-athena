//! `cargo athena init` - scaffold a new workflow crate.
//!
//! Writes a minimal `Cargo.toml`, `src/main.rs`, and `athena.toml` so a
//! new user can go straight to `cargo athena emit`. Interactive on a
//! TTY (prompts for bucket/endpoint/region); flag-driven or
//! all-defaults otherwise.

use std::io::{IsTerminal, Write};
use std::path::PathBuf;
use std::process::exit;

#[derive(clap::Args)]
pub struct InitArgs {
    /// Directory to scaffold (default: current directory)
    ///
    /// Like `cargo init`, refuses if a `Cargo.toml` already exists there.
    #[arg(value_name = "PATH")]
    path: Option<PathBuf>,
    /// Cargo package name. Default: the directory basename.
    #[arg(long)]
    name: Option<String>,
    /// S3 bucket for the binary tarball + artifact ports.
    #[arg(long)]
    bucket: Option<String>,
    /// S3 endpoint (host or `https://host`). Default: `s3.amazonaws.com`.
    #[arg(long)]
    endpoint: Option<String>,
    /// S3 region. Default: `us-east-1`.
    #[arg(long)]
    region: Option<String>,
    /// Non-interactive: accept defaults or flag values, no prompts.
    #[arg(short = 'y', long)]
    yes: bool,
}

pub fn init(args: InitArgs) {
    let dir = match args.path.clone() {
        Some(p) => p,
        None => std::env::current_dir().unwrap_or_else(|e| die(&format!("cwd: {e}"))),
    };
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| die(&format!("mkdir {}: {e}", dir.display())));

    if dir.join("Cargo.toml").exists() {
        die(&format!(
            "{} already contains a Cargo.toml.\n\
             `cargo athena init` only scaffolds new crates. Add cargo-athena \
             to an existing crate with `cargo add cargo-athena --no-default-features`.",
            dir.display()
        ));
    }

    let default_name = args
        .name
        .clone()
        .or_else(|| {
            dir.file_name()
                .and_then(|n| n.to_str())
                .map(sanitize_pkg_name)
        })
        .unwrap_or_else(|| "my-workflow".to_string());

    // Interactive only if stdin is a TTY, -y wasn't passed, and the
    // user didn't fully specify everything via flags.
    let any_flag = args.bucket.is_some() || args.endpoint.is_some() || args.region.is_some();
    let interactive = !args.yes && !any_flag && std::io::stdin().is_terminal();

    let name = if interactive {
        prompt("Package name", &default_name)
    } else {
        default_name
    };
    let bucket = resolve(args.bucket.clone(), "S3 bucket", "my-bucket", interactive);
    let endpoint = resolve(
        args.endpoint.clone(),
        "S3 endpoint",
        "s3.amazonaws.com",
        interactive,
    );
    let region = resolve(args.region.clone(), "S3 region", "us-east-1", interactive);

    let cargo_toml = render_cargo_toml(&name);
    let main_rs = MAIN_RS.to_string();
    let athena_toml = render_athena_toml(&bucket, &endpoint, &region);

    std::fs::create_dir_all(dir.join("src")).unwrap_or_else(|e| die(&format!("mkdir src/: {e}")));
    write_new(&dir.join("Cargo.toml"), &cargo_toml);
    write_new(&dir.join("src/main.rs"), &main_rs);
    write_new(&dir.join("athena.toml"), &athena_toml);

    let here = if args.path.is_some() {
        format!("cd {} && ", dir.display())
    } else {
        String::new()
    };
    eprintln!();
    eprintln!("✓ scaffolded `{name}` in {}", dir.display());
    eprintln!("    Cargo.toml");
    eprintln!("    src/main.rs");
    eprintln!("    athena.toml");
    eprintln!();
    eprintln!("Next:");
    eprintln!("  {here}cargo athena emit            # inspect the YAML");
    eprintln!("  {here}cargo athena publish         # cross-compile + upload the binary");
    eprintln!("  {here}cargo athena submit -y      # run it (source build, root template)");
    eprintln!();
    eprintln!("Need the publish toolchain? Run `cargo athena doctor` to check.");
}

fn render_cargo_toml(name: &str) -> String {
    // Pin to the CLI's own version so the scaffold is guaranteed
    // compatible with the `cargo athena` the user just ran.
    let version = env!("CARGO_PKG_VERSION");
    format!(
        r#"[package]
name = "{name}"
version = "0.1.0"
edition = "2024"

[dependencies]
# Library-only: pulls just the proc macros + runtime, not the CLI tree.
# Install the CLI separately with `cargo install cargo-athena`.
cargo-athena = {{ version = "{version}", default-features = false }}
"#
    )
}

const MAIN_RS: &str = r#"use cargo_athena::{container, workflow};

#[workflow]
fn pipeline() {
    hello("world".to_string());
}

#[container(image = "alpine:3.20")]
fn hello(name: String) {
    println!("hello, {name}!");
}

fn main() {
    cargo_athena::entrypoint!(pipeline);
}
"#;

fn render_athena_toml(bucket: &str, endpoint: &str, region: &str) -> String {
    let insecure_line = if endpoint.contains("amazonaws.com") || endpoint.starts_with("https://") {
        String::new()
    } else {
        // Local MinIO / self-hosted on plain HTTP. Best guess; user
        // can flip it if wrong.
        "insecure = true                                    # plain HTTP (e.g. local MinIO)\n"
            .to_string()
    };
    format!(
        r#"[artifact_repository.s3]
endpoint = "{endpoint}"
bucket = "{bucket}"
region = "{region}"
{insecure_line}access_key_secret = {{ name = "my-s3-creds", key = "accessKey" }}
secret_key_secret = {{ name = "my-s3-creds", key = "secretKey" }}

[bootstrap]
# Cross-compile targets for the workflow binary. Both architectures
# fit in one tarball; the in-pod bootstrap picks the right one.
targets = ["x86_64-unknown-linux-musl", "aarch64-unknown-linux-musl"]

# [defaults]
# package = "..."   # so `cargo athena` doesn't need -p in a workspace
# bin     = "..."   # for multi-bin crates
# namespace        = "argo"   # default kube namespace for `submit`
# service_account  = "default"
"#
    )
}

fn prompt(label: &str, default: &str) -> String {
    print!("  {label} [{default}]: ");
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    let line = line.trim();
    if line.is_empty() {
        default.to_string()
    } else {
        line.to_string()
    }
}

fn resolve(flag: Option<String>, label: &str, default: &str, interactive: bool) -> String {
    if let Some(v) = flag {
        return v;
    }
    if interactive {
        prompt(label, default)
    } else {
        default.to_string()
    }
}

/// Cargo package names must match `^[a-zA-Z][a-zA-Z0-9_-]*$`. The
/// most common dir-name mismatch is leading digits or spaces; sanitize
/// to a kebab-safe form.
fn sanitize_pkg_name(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
            out.push(c.to_ascii_lowercase());
        } else if c.is_whitespace() || c == '.' {
            out.push('-');
        }
        if i == 0 && !out.chars().next().is_some_and(|c| c.is_ascii_alphabetic()) {
            out.insert(0, 'a');
        }
    }
    if out.is_empty() {
        "my-workflow".to_string()
    } else {
        out
    }
}

fn write_new(path: &std::path::Path, content: &str) {
    if path.exists() {
        die(&format!("refusing to overwrite {}", path.display()));
    }
    std::fs::write(path, content)
        .unwrap_or_else(|e| die(&format!("write {}: {e}", path.display())));
}

fn die(msg: &str) -> ! {
    eprintln!("cargo athena init: {msg}");
    exit(2);
}
