//! CLI feedback helpers: step lines, transfer progress bars, spinners.
//!
//! All output goes to stderr so stdout stays scriptable. Style mimics
//! cargo: a leading symbol ("→" for action start, "✓" for done,
//! "✗" for failure), then a description.

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use std::time::Instant;

/// "→ msg" announce, with a `Step` guard that prints "✓ msg (Ns)" on
/// `finish()` or "✗ msg (Ns)" on drop without `finish`. Use the guard
/// to keep timing accurate without sprinkling `Instant::now()` everywhere.
pub fn step(msg: impl Into<String>) -> Step {
    let msg = msg.into();
    eprintln!("→ {msg}");
    Step {
        msg,
        started: Instant::now(),
        finished: false,
    }
}

pub struct Step {
    msg: String,
    started: Instant,
    finished: bool,
}

impl Step {
    /// Print "✓ msg (Ns)" and consume the guard. Idempotent: a no-op
    /// after the first call so a `finish` after error-on-drop is safe.
    pub fn finish(mut self) {
        self.finished = true;
        let secs = self.started.elapsed().as_secs_f32();
        eprintln!("✓ {} ({secs:.1}s)", self.msg);
    }
}

impl Drop for Step {
    fn drop(&mut self) {
        if !self.finished {
            let secs = self.started.elapsed().as_secs_f32();
            eprintln!("✗ {} ({secs:.1}s)", self.msg);
        }
    }
}

/// A bytes-counting progress bar: "[==>    ] 12.3/47.2 MB  5.1 MB/s  ETA 7s".
/// Draws to stderr; auto-hides under a non-TTY (e.g. CI).
pub fn xfer_bar(total: u64, label: &str) -> ProgressBar {
    let bar = ProgressBar::with_draw_target(Some(total), ProgressDrawTarget::stderr());
    bar.set_style(
        ProgressStyle::with_template(
            "  {msg:>12.cyan} [{bar:30.cyan/blue}] {bytes:>10}/{total_bytes:<10} {bytes_per_sec:>12}  ETA {eta:>3}",
        )
        .expect("valid template")
        .progress_chars("=>-"),
    );
    bar.set_message(label.to_string());
    bar
}

/// An indeterminate spinner: "⠋ msg". Use when there's no size known
/// up front (a HEAD check, a brief drift comparison, etc.).
pub fn spinner(msg: impl Into<String>) -> ProgressBar {
    let bar = ProgressBar::with_draw_target(None, ProgressDrawTarget::stderr());
    bar.set_style(ProgressStyle::with_template("  {spinner:.cyan} {msg}").expect("valid template"));
    bar.set_message(msg.into());
    bar.enable_steady_tick(std::time::Duration::from_millis(100));
    bar
}
