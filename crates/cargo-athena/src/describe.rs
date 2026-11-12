//! `cargo athena describe` - print one template's metadata, as the
//! binary reports it in `CARGO_ATHENA_DESCRIBE` mode.

use cargo_athena::{ContainerRunMeta, serde_json};
use std::process::{Stdio, exit};

use crate::binsrc::{BinSel, BinarySource};
use crate::style;

#[derive(clap::Args)]
pub struct DescribeArgs {
    #[command(flatten)]
    bin: BinSel,
    /// Template to describe
    ///
    /// `<crate>-<fn>` kebab, or the `#[container(name = "…")]` override.
    /// Default: the binary's root template.
    #[arg(short = 'w', long = "workflow", value_name = "TEMPLATE")]
    workflow: Option<String>,
    /// Print the raw `ContainerRunMeta` JSON (scriptable)
    ///
    /// Default is a short human-readable summary listing inputs + resources.
    #[arg(long)]
    json: bool,
}

/// `cargo athena describe [BINARY] [-w TEMPLATE]` - print one template's
/// metadata (inputs, image, resources), for either a `#[container]` or a
/// `#[workflow]`. Defaults to the binary's root template.
pub fn describe(a: DescribeArgs) {
    let src = a.bin.resolve();
    let info = src.probe();
    let template = a.workflow.clone().unwrap_or(info.default_template);
    let meta = describe_meta(&src, &template);
    if a.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&meta).expect("ContainerRunMeta is serializable")
        );
    } else {
        print_human(&meta);
    }
}

/// Human-readable describe output: a flat metadata block, then the
/// Rust-style fn signature, then a copy-pasteable submit command. The
/// raw JSON form (back-compat for scripts) is one `--json` away.
///
/// Coloring goes through the shared `style` palette (`console`-backed, so
/// it auto-disables off-TTY / under `NO_COLOR`, keeping piped output
/// clean); two describe-specific header accents stay local.
fn print_human(m: &ContainerRunMeta) {
    use console::Style;
    // `style::*` is stdout-targeted (we write to stdout exclusively here).
    // The kind accent (cyan + bold) and the `fn` keyword (magenta) are
    // describe-local, so they're built inline.
    let kind_s = Style::new().cyan().bold();
    let sig_kw = Style::new().magenta();
    let name_s = style::name();
    let pkg_s = style::label();
    let label_s = style::label();
    let cmd_s = style::cmd();

    let kind = if m.kind.is_empty() {
        "template"
    } else {
        &m.kind
    };
    // Default emit names are `<crate>-<fn>`; show just the short form
    // here (the package is right next to it). Overridden names without
    // that prefix stay verbatim.
    let pfx = format!("{}-", m.package);
    let short = m.name.strip_prefix(&pfx).unwrap_or(&m.name);

    // Header: `<kind> <name>  (<package>)`
    println!();
    let head_pkg = if m.package.is_empty() {
        String::new()
    } else {
        format!("  {}", pkg_s.apply_to(format!("({})", m.package)))
    };
    println!(
        "{} {}{head_pkg}",
        kind_s.apply_to(kind),
        name_s.apply_to(short)
    );
    println!();

    // Flat metadata block - only the lines that are populated. Aligned
    // column so the values line up.
    let mut rows: Vec<(&str, String)> = Vec::new();
    if m.kind == "container" && !m.image.is_empty() {
        rows.push(("image", m.image.clone()));
    }
    if !m.host_paths.is_empty() {
        rows.push(("host mounts", m.host_paths.join(", ")));
    }
    if !m.input_artifacts.is_empty() {
        let keys: Vec<String> = m.input_artifacts.iter().map(|a| a.s3.key.clone()).collect();
        rows.push(("input artifacts", keys.join(", ")));
    }
    if !m.output_artifacts.is_empty() {
        let keys: Vec<String> = m
            .output_artifacts
            .iter()
            .map(|a| a.s3.key.clone())
            .collect();
        rows.push(("output artifacts", keys.join(", ")));
    }
    if !rows.is_empty() {
        let w = rows.iter().map(|(k, _)| k.len()).max().unwrap_or(0);
        for (k, v) in &rows {
            println!("  {}  {v}", label_s.apply_to(format!("{k:<w$}")));
        }
        println!();
    }

    // Rust-style fn signature - the same shape `ls` shows as
    // `SIGNATURE`, but with the leading `fn <name>`.
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
    println!(
        "  {} {}({inner})",
        sig_kw.apply_to("fn"),
        name_s.apply_to(short)
    );

    // Copy-pasteable submit line. Short name (submit accepts either).
    println!();
    let cmd = if m.params.is_empty() {
        format!("cargo athena submit <BINARY> -w {short}")
    } else {
        let args: Vec<String> = m
            .params
            .iter()
            .map(|p| {
                let ty = if p.ty.is_empty() {
                    "?".into()
                } else {
                    p.ty.clone()
                };
                format!("-a {}=<{}>", p.name, ty)
            })
            .collect();
        format!("cargo athena submit <BINARY> -w {short} {}", args.join(" "))
    };
    println!("  {} {}", label_s.apply_to("$"), cmd_s.apply_to(cmd));
    println!();
}

fn die(msg: &str) -> ! {
    eprintln!("cargo athena describe: {msg}");
    exit(2);
}

/// Run the workflow binary in describe-mode and parse one template's
/// metadata. The caller has already [`probe`](BinarySource::probe)d the
/// source, so a failure here means the named template isn't reachable.
fn describe_meta(src: &BinarySource, template: &str) -> ContainerRunMeta {
    let hint = || -> String {
        format!(
            "could not get metadata for {template:?} from the workflow binary.\n\
             \x20 - {template:?} must be a template reachable from the binary's root \
             (`<crate>-<fn>` kebab, or the #[container(name=…)] override);\n\
             \x20 - `cargo athena ls` shows what the binary exposes."
        )
    };
    let out = src
        .command()
        .env("CARGO_ATHENA_DESCRIBE", template)
        // Stream the binary's stderr to the user; capture stdout (JSON).
        .stderr(Stdio::inherit())
        .stdout(Stdio::piped())
        .output()
        .unwrap_or_else(|e| {
            die(&format!(
                "failed to run the workflow binary: {e}\n{}",
                hint()
            ))
        });
    if !out.status.success() || out.stdout.is_empty() {
        let err = String::from_utf8_lossy(&out.stderr);
        let tail: String = err
            .lines()
            .rev()
            .take(8)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .map(|l| format!("    {l}"))
            .collect::<Vec<_>>()
            .join("\n");
        die(&format!("{}\n\n  binary output (tail):\n{tail}", hint()));
    }
    serde_json::from_slice(&out.stdout).unwrap_or_else(|e| {
        die(&format!(
            "could not parse container metadata ({e}).\n{}",
            hint()
        ))
    })
}
