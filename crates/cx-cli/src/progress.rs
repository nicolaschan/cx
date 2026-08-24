//! Progress on stderr, sized in bytes to compress so the ETA tracks
//! wall-clock.

use std::time::Duration;

use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};

/// Whether progress may draw at all: the caller's answer to "is stderr a
/// terminal?", decided once, the way color is for stdout.
#[derive(Clone, Copy, Default)]
pub struct Progress {
    pub visible: bool,
}

impl Progress {
    /// A bar over `cost` bytes: the returned sink advances it, and
    /// dropping the sink clears it.
    pub fn phase(self, label: &'static str, cost: u64) -> impl Fn(u64) + Sync {
        let bar = if self.visible {
            let style = ProgressStyle::with_template(
                " {spinner:.dim} {msg:.dim} {wide_bar} {percent:>3}% {eta:.dim}",
            )
            .expect("static template")
            .progress_chars("██░");
            let bar = ProgressBar::with_draw_target(Some(cost), ProgressDrawTarget::stderr())
                .with_style(style)
                .with_message(label);
            bar.enable_steady_tick(Duration::from_millis(100));
            bar
        } else {
            ProgressBar::hidden()
        };
        move |bytes| bar.inc(bytes)
    }
}
