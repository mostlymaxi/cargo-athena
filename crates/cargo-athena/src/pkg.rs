//! Cargo `--package` / `--bin` selection, shared by every `cargo athena`
//! subcommand. Resolves in this order:
//!
//!   1. `-p` / `--package` and `--bin` flags
//!   2. `[defaults].package` / `.bin` in `athena.toml`
//!   3. cargo's single-package / default-bin autodetect (caller's job;
//!      `resolve` just returns `None` here)

use cargo_athena::AthenaConfig;

#[derive(clap::Args, Clone)]
pub struct PkgSel {
    /// Cargo package to drive. Default: `[defaults].package` in
    /// athena.toml, else the sole workspace package.
    #[arg(short = 'p', long)]
    package: Option<String>,
    /// Cargo bin within it. Default: `[defaults].bin`, else the
    /// package's default bin.
    #[arg(long)]
    bin: Option<String>,
}

impl PkgSel {
    /// Constructor for code paths that parse `--package` / `--bin`
    /// without going through the derive (e.g. `submit`'s two-stage
    /// parser, which pulls these flags out manually in phase 1).
    pub(crate) fn from_flags(package: Option<String>, bin: Option<String>) -> Self {
        Self { package, bin }
    }

    pub(crate) fn resolve(&self) -> (Option<String>, Option<String>) {
        let d = AthenaConfig::load().defaults;
        (
            self.package.clone().or(d.package),
            self.bin.clone().or(d.bin),
        )
    }
}
