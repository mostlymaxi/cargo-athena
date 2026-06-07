//! Shared CLI color palette: one place so every subcommand looks the same.
//!
//! Built on `console::Style`, which auto-disables ANSI when the target
//! stream isn't a TTY or `NO_COLOR` is set, so piped / CI output stays
//! clean. Status symbols + summaries target **stderr** (where progress and
//! checks print, keeping stdout scriptable); the table / describe styles
//! target **stdout** (their data stream).

use console::{Style, StyledObject};

// --- status symbols (stderr: step lines, doctor checks) -------------------

/// Green check: a completed step or a passing check.
pub fn ok() -> StyledObject<&'static str> {
    Style::new().for_stderr().green().apply_to("\u{2713}")
}

/// Red cross: a failed step or check.
pub fn err() -> StyledObject<&'static str> {
    Style::new().for_stderr().red().apply_to("\u{2717}")
}

/// Yellow question mark: a soft warning, not a hard failure.
pub fn warn() -> StyledObject<&'static str> {
    Style::new().for_stderr().yellow().apply_to("?")
}

/// Cyan arrow: a step in progress.
pub fn arrow() -> StyledObject<&'static str> {
    Style::new().for_stderr().cyan().apply_to("\u{2192}")
}

/// Green / red stderr text, for summary lines.
pub fn good() -> Style {
    Style::new().for_stderr().green()
}
pub fn bad() -> Style {
    Style::new().for_stderr().red()
}

// --- stdout text (ls table, describe block) -------------------------------

/// Dim + bold: table column headers.
pub fn header() -> Style {
    Style::new().dim().bold()
}

/// Dim: secondary text (package column, field labels, the `$` prompt).
pub fn label() -> Style {
    Style::new().dim()
}

/// Bold: a primary identifier (template name).
pub fn name() -> Style {
    Style::new().bold()
}

/// Green: a copy-pasteable command.
pub fn cmd() -> Style {
    Style::new().green()
}

/// Per-template-kind accent: cyan container, magenta workflow, else plain.
pub fn kind(kind: &str) -> Style {
    match kind {
        "container" => Style::new().cyan(),
        "workflow" => Style::new().magenta(),
        _ => Style::new(),
    }
}
