//! `cargo athena` — drives a user crate's cargo-athena binary.
//!
//! The entrypoint is fixed in the user binary's `main`
//! (`entrypoint::<Root>()`), so this subcommand just runs that binary in
//! the right mode and relays its output.
//!
//!   cargo athena build  [--package P] [--bin B] [--out FILE]
//!   cargo athena run    --template <template-name> [--package P] [--bin B] [--input JSON]

use std::process::{Command, exit};

fn main() {
    // When invoked as a cargo subcommand argv is `cargo-athena athena <args...>`.
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(String::as_str) == Some("athena") {
        args.remove(0);
    }

    let sub = args.first().cloned().unwrap_or_default();
    let rest = &args[1.min(args.len())..];

    match sub.as_str() {
        "build" => build(rest),
        "run" => run(rest),
        _ => {
            eprintln!(
                "usage:\n  cargo athena build [--package P] [--bin B] [--out FILE]\n  \
                 cargo athena run --template <template-name> [--package P] [--bin B] [--input JSON]"
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

fn build(args: &[String]) {
    let cmd = &mut cargo_run(args);
    let out = cmd.output().expect("failed to run user binary");
    if !out.status.success() {
        eprint!("{}", String::from_utf8_lossy(&out.stderr));
        exit(out.status.code().unwrap_or(1));
    }
    match opt(args, "--out") {
        Some(path) => {
            std::fs::write(path, &out.stdout).expect("write --out file");
            eprintln!("wrote {path}");
        }
        None => {
            print!("{}", String::from_utf8_lossy(&out.stdout));
        }
    }
}

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
