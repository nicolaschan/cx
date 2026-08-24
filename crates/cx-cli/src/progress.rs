//! Progress on stderr, drawn by indicatif. A phase is sized in bytes to
//! compress rather than files, so the bar and its ETA track wall-clock.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Whether progress may draw at all: the caller's answer to "is stderr a
/// terminal?", decided once, the way color is for stdout.
#[derive(Clone, Copy)]
pub struct Progress {
    pub visible: bool,
}

impl Progress {
    pub fn hidden() -> Self {
        Progress { visible: false }
    }

    /// A bar over `cost` bytes of work.
    pub fn phase(self, label: &'static str, cost: u64) -> Phase {
        self.start(
            label,
            Some(cost),
            " {spinner:.dim} {msg:.dim} {wide_bar} {percent:>3}% {eta:.dim}",
        )
    }

    /// A spinner for one indivisible job.
    pub fn spinner(self, label: &'static str) -> Phase {
        self.start(label, None, " {spinner:.dim} {msg:.dim} {elapsed:.dim}")
    }

    fn start(self, label: &'static str, len: Option<u64>, template: &str) -> Phase {
        if !self.visible {
            return Phase(ProgressBar::hidden());
        }
        let style = ProgressStyle::with_template(template)
            .expect("static template")
            .progress_chars("██░");
        let bar = ProgressBar::with_draw_target(len, ProgressDrawTarget::stderr())
            .with_style(style)
            .with_message(label);
        bar.enable_steady_tick(Duration::from_millis(100));
        Phase(bar)
    }
}

/// One running phase's bar: advances from any thread, clears itself when
/// dropped.
pub struct Phase(ProgressBar);

impl cx_core::Progress for Phase {
    fn advance(&self, bytes: u64) {
        self.0.inc(bytes);
    }
}
