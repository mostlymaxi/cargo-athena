//! Binary-first program selection for the consumer commands (`emit`,
//! `ls`, `describe`, `emulate`, `submit`). The subject is a *binary*: a
//! prebuilt cargo-athena executable (a path, or a name on `$PATH` from
//! `cargo install`), so these commands need no source checkout. Passing
//! no binary falls back to building from the current crate (or
//! `--manifest-path`) - the developer loop.
//!
//! Every consumer routes its metadata reads (`CARGO_ATHENA_LIST` /
//! `DESCRIBE` / `EMIT_JSON`) through [`BinarySource::command`], and first
//! runs [`BinarySource::probe`] to confirm the executable really is a
//! cargo-athena binary and that its wire protocol matches this CLI.
//! `build` / `publish` are NOT consumers - they need source and keep
//! `PkgSel`.

use cargo_athena::{ATHENA_PROBE_KIND, ATHENA_PROTOCOL, AthenaConfig, ProbeInfo, serde_json};
use std::path::PathBuf;
use std::process::{Command, Stdio, exit};

/// How a consumer command names the program to act on: a prebuilt binary
/// (positional), or - absent that - a source build.
#[derive(clap::Args)]
pub struct BinSel {
    /// A cargo-athena binary: a path, or a bare name resolved on `$PATH`
    /// (e.g. one installed with `cargo install`). Omit to build from
    /// source instead (the current crate, or `--manifest-path`).
    #[arg(value_name = "BINARY")]
    binary: Option<String>,
    /// Build from source instead of running a prebuilt binary: the crate
    /// (a directory or its `Cargo.toml`) to build. Defaults to the cwd.
    #[arg(long = "manifest-path", value_name = "PATH", conflicts_with = "binary")]
    manifest_path: Option<PathBuf>,
    /// (source build) cargo package to build / drive.
    #[arg(short = 'p', long, conflicts_with = "binary")]
    package: Option<String>,
    /// (source build) cargo bin within the package.
    #[arg(long = "bin", conflicts_with = "binary")]
    bin: Option<String>,
}

/// The resolved program a consumer command runs.
pub enum BinarySource {
    /// A prebuilt executable: a path, or a `$PATH`-resolved name.
    Exe(String),
    /// Build + run from source via `cargo run`.
    Cargo {
        manifest_path: Option<PathBuf>,
        package: Option<String>,
        bin: Option<String>,
    },
}

impl BinSel {
    pub(crate) fn resolve(&self) -> BinarySource {
        if let Some(b) = &self.binary {
            // Prebuilt binary: read its build-time-sealed tag as-is — do
            // NOT inject one (the binary is the source of truth).
            return BinarySource::Exe(b.clone());
        }
        // Source build: the CLI is about to compile the binary, so seal the
        // SAME version tag `build`/`publish` would (a dev tree -> dev-<commit>),
        // so emit/submit names + the S3 key match a prior `publish` without
        // the user exporting ATHENA_VERSION_TAG. Done before any `cargo run`
        // below so the compile bakes it.
        crate::gitinfo::export_source_build_tag();
        // Package/bin fall back to `[defaults]` in athena.toml (parity with
        // the old PkgSel); only consulted on this path, never in the
        // source-free Exe path above.
        let d = AthenaConfig::load().defaults;
        BinarySource::Cargo {
            manifest_path: self.manifest_path.clone(),
            package: self.package.clone().or(d.package),
            bin: self.bin.clone().or(d.bin),
        }
    }
}

impl BinarySource {
    /// A `Command` that runs the user binary. The caller adds the
    /// `CARGO_ATHENA_*` mode env var and wires stdio. NO `--quiet` on the
    /// cargo path: "Compiling..." progress and compile errors stream to
    /// the user's terminal (stderr).
    pub(crate) fn command(&self) -> Command {
        match self {
            // Bare name -> `$PATH` lookup; a path -> run directly.
            BinarySource::Exe(p) => Command::new(p),
            BinarySource::Cargo {
                manifest_path,
                package,
                bin,
            } => {
                let mut c = Command::new("cargo");
                c.arg("run");
                if let Some(m) = manifest_path {
                    let mp = if m.is_dir() {
                        m.join("Cargo.toml")
                    } else {
                        m.clone()
                    };
                    c.arg("--manifest-path").arg(mp);
                }
                if let Some(p) = package {
                    c.args(["--package", p]);
                }
                if let Some(b) = bin {
                    c.args(["--bin", b]);
                }
                c
            }
        }
    }

    /// Human label for diagnostics.
    fn label(&self) -> String {
        match self {
            BinarySource::Exe(p) => p.clone(),
            BinarySource::Cargo { .. } => "the workflow crate".to_string(),
        }
    }

    /// The cargo `(package, bin)` selectors, or `None` for a prebuilt
    /// binary. Used by the few source-only sub-operations (e.g. emulate's
    /// `--build`) that can't run against a prebuilt binary.
    pub(crate) fn cargo_pkg_bin(&self) -> Option<(Option<String>, Option<String>)> {
        match self {
            BinarySource::Exe(_) => None,
            BinarySource::Cargo { package, bin, .. } => Some((package.clone(), bin.clone())),
        }
    }

    /// Confirm this is a cargo-athena binary and that its metadata wire
    /// protocol matches this CLI; return its [`ProbeInfo`]. Run FIRST by
    /// every consumer (the binary may be arbitrary / installed / a skewed
    /// version), so failures read clearly instead of as a downstream serde
    /// panic. Config-free: the binary's PROBE mode reads no athena.toml.
    pub(crate) fn probe(&self) -> ProbeInfo {
        let out = self
            .command()
            .env("CARGO_ATHENA_PROBE", "1")
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .output()
            .unwrap_or_else(|e| die(&format!("failed to run {}: {e}", self.label())));
        if !out.status.success() {
            die(&format!(
                "{} did not respond to a cargo-athena probe (exit {:?}). Is it a binary built \
                 with cargo-athena (its `main` calls `cargo_athena::entrypoint!(Root)`)?",
                self.label(),
                out.status.code()
            ));
        }
        // Check the two handshake fields off a tolerant `Value` FIRST (no
        // serde derive needed), so a `kind`/protocol mismatch reports a
        // clear message instead of failing the full `ProbeInfo` parse. The
        // full parse must not gate the handshake check: a struct mismatch at
        // a matching protocol is a CLI/library version skew, which we want
        // to name precisely.
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap_or_else(|_| {
            die(&format!(
                "{} does not look like a cargo-athena binary (unrecognized probe response).",
                self.label()
            ))
        });
        if v.get("kind").and_then(|k| k.as_str()) != Some(ATHENA_PROBE_KIND) {
            die(&format!("{} is not a cargo-athena binary.", self.label()));
        }
        let proto = v
            .get("athena_protocol")
            .and_then(|p| p.as_u64())
            .unwrap_or(0) as u32;
        if proto != ATHENA_PROTOCOL {
            let hint = if proto > ATHENA_PROTOCOL {
                "upgrade the CLI (`cargo install cargo-athena`)"
            } else {
                "rebuild the workflow binary against this cargo-athena (its library + the CLI must match), or use a matching CLI"
            };
            die(&format!(
                "version mismatch: {} speaks cargo-athena probe protocol {proto}, this CLI speaks {ATHENA_PROTOCOL} ({hint}).",
                self.label(),
            ));
        }
        // kind + protocol matched, so this IS a cargo-athena binary; if the
        // full struct still doesn't parse, the binary and this CLI were
        // built from struct-incompatible cargo-athena versions (a dev skew,
        // e.g. a stale path-dependency).
        serde_json::from_value(v).unwrap_or_else(|_| {
            die(&format!(
                "{} was built with a different cargo-athena than this CLI (its \
                 probe is missing fields the CLI expects). Rebuild the workflow \
                 binary against the same cargo-athena — its library dependency \
                 and the `cargo athena` CLI must be the same version.",
                self.label()
            ))
        })
    }

    /// Run a `CARGO_ATHENA_*` metadata mode; return its stdout (the JSON
    /// payload). The caller `from_slice`s it into the concrete type.
    pub(crate) fn run_mode(&self, env: &str, val: &str, what: &str) -> Vec<u8> {
        let out = self
            .command()
            .env(env, val)
            .stderr(Stdio::inherit())
            .stdout(Stdio::piped())
            .output()
            .unwrap_or_else(|e| die(&format!("failed to run {}: {e}", self.label())));
        if !out.status.success() || out.stdout.is_empty() {
            die(&format!("could not get {what} from {}", self.label()));
        }
        out.stdout
    }
}

fn die(m: &str) -> ! {
    eprintln!("cargo athena: {m}");
    exit(2);
}
