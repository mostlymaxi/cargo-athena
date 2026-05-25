//! `cargo athena container ls` / `cargo athena workflow ls` — list the
//! templates a workflow binary reports. Sibling to `emulate` (which runs
//! one container locally) and to the `describe` subcommands; spawns the
//! user binary in `CARGO_ATHENA_LIST` mode and renders a small table.
//!
//! * `container ls` filters to `#[container]`s (the things `container
//!   emulate` can drive).
//! * `workflow ls` is the more general view: every reachable template,
//!   container or workflow. Synthetic `if`/`else` wrappers + arms are
//!   hidden unless `--include-synthetic` (an implementation detail of
//!   how the macros lower control flow).

use cargo_athena::{ContainerRunMeta, serde_json};
use std::process::{Stdio, exit};

use crate::pkg::PkgSel;

#[derive(clap::Args)]
pub struct LsArgs {
    #[command(flatten)]
    pkg: PkgSel,
}

#[derive(clap::Args)]
pub struct WorkflowLsArgs {
    #[command(flatten)]
    pkg: PkgSel,
    /// Also list athena-synthesized `if`/`else` wrapper + arm
    /// sub-workflows (an implementation detail, hidden by default).
    #[arg(long)]
    include_synthetic: bool,
}

/// `cargo athena container ls` - the `#[container]`s in the package
/// (the things `container emulate` runs). For a wider view that also
/// includes `#[workflow]`s, run `cargo athena workflow ls`.
pub fn container_ls(a: LsArgs) {
    let (pkg, bin) = a.pkg.resolve();
    let all = fetch_list(pkg.as_deref(), bin.as_deref());
    print_table(all.iter().filter(|m| m.kind == "container").collect());
}

/// `cargo athena workflow ls` - every reachable template, both
/// `#[container]`s and `#[workflow]`s (workflow is the more general
/// view). Synthetic `if`/`else` wrappers are hidden unless
/// `--include-synthetic`.
pub fn workflow_ls(a: WorkflowLsArgs) {
    let (pkg, bin) = a.pkg.resolve();
    let all = fetch_list(pkg.as_deref(), bin.as_deref());
    print_table(
        all.iter()
            .filter(|m| a.include_synthetic || !m.synthetic)
            .collect(),
    );
}

/// Spawn the workflow binary in list-mode and parse every template's
/// metadata.
fn fetch_list(pkg: Option<&str>, bin: Option<&str>) -> Vec<ContainerRunMeta> {
    let mut cmd = crate::cargo_run(pkg, bin);
    cmd.env("CARGO_ATHENA_LIST", "1");
    // Stream cargo's "Compiling..." progress to the user's terminal.
    cmd.stderr(Stdio::inherit());
    let out = cmd
        .output()
        .unwrap_or_else(|e| die(&format!("failed to spawn `cargo run`: {e}")));
    if !out.status.success() || out.stdout.is_empty() {
        die("could not list templates (run from your workflow crate, or pass --package/--bin)");
    }
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| die(&format!("could not parse template list ({e})")))
}

/// Render a `PACKAGE  NAME  KIND  SIGNATURE` table (rows pre-filtered +
/// sorted). The default emitted name is `<crate>-<fn>`; we strip the
/// package prefix in the `NAME` column since it's already shown in
/// `PACKAGE` (overridden `#[container(name=…)]` names without that
/// prefix are left as-is). `SIGNATURE` is Rust fn syntax
/// `(name: Type, …)` so each row already tells you exactly how to
/// invoke it.
///
/// Coloring uses `console::Style`, which auto-disables when stdout
/// isn't a TTY or `NO_COLOR` is set, so piped output stays clean.
fn print_table(mut rows: Vec<&ContainerRunMeta>) {
    use console::Style;
    rows.sort_by(|x, y| x.name.cmp(&y.name));
    if rows.is_empty() {
        eprintln!("(no matching templates)");
        return;
    }
    let header_s = Style::new().dim().bold();
    let pkg_s = Style::new().dim();
    let name_s = Style::new().bold();
    let kind_container = Style::new().cyan();
    let kind_workflow = Style::new().magenta();
    let kind_other = Style::new();

    let short = |m: &ContainerRunMeta| -> String {
        let pfx = format!("{}-", m.package);
        m.name.strip_prefix(&pfx).unwrap_or(&m.name).to_string()
    };
    let sig = |m: &ContainerRunMeta| -> String {
        let inner = m
            .params
            .iter()
            .map(|p| {
                if p.ty.is_empty() {
                    p.name.clone()
                } else {
                    format!("{}: {}", p.name, p.ty)
                }
            })
            .collect::<Vec<_>>()
            .join(", ");
        format!("({inner})")
    };
    let pw = rows
        .iter()
        .map(|m| m.package.len())
        .max()
        .unwrap_or(7)
        .max(7);
    let nw = rows
        .iter()
        .map(|m| short(m).len())
        .max()
        .unwrap_or(4)
        .max(4);
    println!(
        "{}  {}  {}  {}",
        header_s.apply_to(format!("{:<pw$}", "PACKAGE", pw = pw)),
        header_s.apply_to(format!("{:<nw$}", "NAME", nw = nw)),
        header_s.apply_to("KIND     "),
        header_s.apply_to("SIGNATURE"),
    );
    for m in rows {
        let kind_s = match m.kind.as_str() {
            "container" => &kind_container,
            "workflow" => &kind_workflow,
            _ => &kind_other,
        };
        println!(
            "{}  {}  {}  {}",
            pkg_s.apply_to(format!("{:<pw$}", m.package, pw = pw)),
            name_s.apply_to(format!("{:<nw$}", short(m), nw = nw)),
            kind_s.apply_to(format!("{:<9}", m.kind)),
            sig(m),
        );
    }
}

fn die(msg: &str) -> ! {
    eprintln!("cargo athena ls: {msg}");
    exit(2);
}
