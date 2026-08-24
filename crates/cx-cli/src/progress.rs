//! Progress on stderr while scoring runs, drawn by indicatif. A phase is
//! sized in the bytes zstd will index — the unit cx-core reports in — so
//! the bar and its ETA track wall-clock rather than file count.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressFinish, ProgressStyle};

/// Whether progress may draw at all: the caller's answer to "is stderr a
/// terminal?", decided once, the way color is for stdout.
#[derive(Clone, Copy, Default)]
pub struct Progress {
    visible: bool,
}

impl Progress {
    pub fn new(visible: bool) -> Self {
        Progress { visible }
    }

    pub fn hidden() -> Self {
        Self::new(false)
    }

    /// A bar over `cost` bytes of work; clears itself when dropped.
    pub fn phase(self, label: &str, cost: u64) -> Phase {
        self.start(
            label,
            Some(cost),
            " {spinner:.dim} {msg:.dim} {wide_bar} {percent:>3}% {eta:.dim}",
        )
    }

    /// A spinner for one indivisible job; clears itself when dropped.
    pub fn spinner(self, label: &str) -> Phase {
        self.start(label, None, " {spinner:.dim} {msg:.dim} {elapsed:.dim}")
    }

    fn start(self, label: &str, len: Option<u64>, template: &str) -> Phase {
        if !self.visible {
            return Phase(ProgressBar::hidden());
        }
        let style = ProgressStyle::with_template(template)
            .expect("static template")
            .progress_chars("██░");
        let bar = ProgressBar::with_draw_target(len, ProgressDrawTarget::stderr())
            .with_style(style)
            .with_message(label.to_owned())
            .with_finish(ProgressFinish::AndClear);
        bar.enable_steady_tick(Duration::from_millis(100));
        Phase(bar)
    }
}

/// One running phase's bar; advances from any thread.
pub struct Phase(ProgressBar);

impl cx_core::Progress for Phase {
    fn advance(&self, bytes: u64) {
        self.0.inc(bytes);
    }
}
