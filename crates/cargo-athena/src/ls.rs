//! `cargo athena ls` - list the templates a workflow binary exposes.
//! Sibling to `emulate` (runs one container locally) and `describe`
//! (one template's metadata); runs the binary in `CARGO_ATHENA_LIST`
//! mode and renders a small table.
//!
//! Lists every reachable template - `#[container]`s and `#[workflow]`s -
//! by default. `--kind container|workflow` narrows it. Synthetic
//! `if`/`else` wrappers + arms (an implementation detail of how the
//! macros lower control flow) are hidden unless `--include-synthetic`.

use cargo_athena::{ContainerRunMeta, serde_json};
use std::process::exit;

use crate::binsrc::{BinSel, BinarySource};
use crate::style;

#[derive(Clone, Copy, clap::ValueEnum)]
pub enum KindFilter {
    Container,
    Workflow,
}

#[derive(clap::Args)]
pub struct LsArgs {
    #[command(flatten)]
    bin: BinSel,
    /// Only show this template kind (default: both).
    #[arg(long, value_enum)]
    kind: Option<KindFilter>,
    /// Also list athena-synthesized `if`/`else` wrapper + arm
    /// sub-workflows (an implementation detail, hidden by default).
    #[arg(long)]
    include_synthetic: bool,
}

/// `cargo athena ls` - every reachable template the binary exposes,
/// optionally filtered to one `--kind`. Synthetic `if`/`else` wrappers
/// are hidden unless `--include-synthetic`.
pub fn ls(a: LsArgs) {
    let src = a.bin.resolve();
    src.probe();
    let all = fetch_list(&src);
    let want = match a.kind {
        Some(KindFilter::Container) => Some("container"),
        Some(KindFilter::Workflow) => Some("workflow"),
        None => None,
    };
    print_table(
        all.iter()
            .filter(|m| a.include_synthetic || !m.synthetic)
            .filter(|m| want.is_none_or(|k| m.kind == k))
            .collect(),
    );
}

/// Run the workflow binary in list-mode and parse every template's
/// metadata.
fn fetch_list(src: &BinarySource) -> Vec<ContainerRunMeta> {
    let out = src.run_mode("CARGO_ATHENA_LIST", "1", "template list");
    serde_json::from_slice(&out)
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
/// Coloring goes through the shared `style` palette (`console`-backed, so
/// it auto-disables when stdout isn't a TTY or `NO_COLOR` is set, keeping
/// piped output clean).
fn print_table(mut rows: Vec<&ContainerRunMeta>) {
    rows.sort_by(|x, y| x.name.cmp(&y.name));
    if rows.is_empty() {
        eprintln!("(no matching templates)");
        return;
    }

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
        style::header().apply_to(format!("{:<pw$}", "PACKAGE", pw = pw)),
        style::header().apply_to(format!("{:<nw$}", "NAME", nw = nw)),
        style::header().apply_to("KIND     "),
        style::header().apply_to("SIGNATURE"),
    );
    for m in rows {
        println!(
            "{}  {}  {}  {}",
            style::label().apply_to(format!("{:<pw$}", m.package, pw = pw)),
            style::name().apply_to(format!("{:<nw$}", short(m), nw = nw)),
            style::kind(&m.kind).apply_to(format!("{:<9}", m.kind)),
            sig(m),
        );
    }
}

fn die(msg: &str) -> ! {
    eprintln!("cargo athena ls: {msg}");
    exit(2);
}
