//! Pure scoring engine: marginal description length of byte strings,
//! estimated by zstd compression conditioned on a reference.
//!
//! No git, no filesystem, no I/O, no threads — callers supply bytes.
//! This is the WASM-safe boundary; everything here is a pure function of
//! its inputs.
//!
//! Scores are comparable only within one compressor version + parameter
//! set. Callers should surface [`zstd_version`] next to any scores.

pub mod testgen;

use zstd_safe::zstd_sys::ZSTD_EndDirective::ZSTD_e_flush;
use zstd_safe::{CCtx, CParameter, InBuffer, OutBuffer};

/// The zstd library version this build scores with, e.g. "1.5.6".
pub fn zstd_version() -> String {
    let n = zstd_safe::version_number();
    format!("{}.{}.{}", n / 10000, (n / 100) % 100, n % 100)
}

pub struct Scorer {
    level: i32,
    max_window_log: u32,
}

impl Default for Scorer {
    fn default() -> Self {
        Self::new(
            19,
            if cfg!(target_pointer_width = "64") {
                31
            } else {
                27
            },
        )
    }
}

impl Scorer {
    pub fn level(&self) -> i32 {
        self.level
    }

    pub fn max_window_log(&self) -> u32 {
        self.max_window_log
    }

    pub fn new(level: i32, max_window_log: u32) -> Self {
        Scorer {
            level,
            max_window_log,
        }
    }

    /// C(input | reference).
    pub fn score(&self, reference: &[&[u8]], input: &[u8]) -> u64 {
        self.attribution(reference, &[input]).run(|_| {})[0]
    }

    pub fn attribution<'a>(
        &'a self,
        reference: &'a [&'a [u8]],
        items: &'a [&'a [u8]],
    ) -> Attribution<'a> {
        Attribution {
            scorer: self,
            reference,
            items,
        }
    }

    /// Smallest window covering the whole reference + input, clamped to
    /// zstd's floor of 10 and this scorer's platform ceiling.
    fn window_log(&self, stream_len: u64) -> u32 {
        let needed = u64::BITS - stream_len.saturating_sub(1).leading_zeros();
        needed.clamp(10, self.max_window_log)
    }
}

/// One zstd stream — reference, then items — flushed at every part
/// boundary, so an item's score is the bytes its part added:
/// C(item_i | reference ++ items[..i]), sequential chain-rule scoring, so
/// a pattern repeated across items is charged to its first occurrence and
/// near-free afterwards.
pub struct Attribution<'a> {
    scorer: &'a Scorer,
    reference: &'a [&'a [u8]],
    items: &'a [&'a [u8]],
}

impl Attribution<'_> {
    /// Bytes to compress: the unit `progress` advances in.
    pub fn bytes(&self) -> u64 {
        self.reference
            .iter()
            .chain(self.items)
            .map(|part| part.len() as u64)
            .sum()
    }

    /// Each item's score; `progress` receives each part's length as it
    /// finishes, reference parts included.
    pub fn run(&self, progress: impl Fn(u64)) -> Vec<u64> {
        let mut cctx = CCtx::create();
        for p in [
            CParameter::CompressionLevel(self.scorer.level),
            // Determinism by construction, not by libzstd's default: MT zstd
            // changes output sizes.
            CParameter::NbWorkers(0),
            CParameter::EnableLongDistanceMatching(true),
            CParameter::WindowLog(self.scorer.window_log(self.bytes())),
        ] {
            cctx.set_parameter(p).expect("static zstd parameter");
        }

        let mut out: Vec<u8> = Vec::new();
        let mut feed = |part: &[u8]| -> u64 {
            out.clear();
            let mut source = InBuffer::around(part);
            loop {
                // Room for one more block so every call makes progress;
                // zstd reports 0 once the part is consumed and flushed.
                out.reserve(CCtx::out_size());
                let pos = out.len();
                let mut sink = OutBuffer::around_pos(&mut out, pos);
                let pending = cctx
                    .compress_stream2(&mut sink, &mut source, ZSTD_e_flush)
                    .unwrap_or_else(|c| panic!("zstd: {}", zstd_safe::get_error_name(c)));
                if pending == 0 {
                    break;
                }
            }
            progress(part.len() as u64);
            out.len() as u64
        };
        for part in self.reference {
            feed(part);
        }
        self.items.iter().map(|item| feed(item)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_scores_zero() {
        let scorer = Scorer::default();
        assert_eq!(scorer.score(&[b"reference"], &[]), 0);
    }

    #[test]
    fn window_log_covers_stream() {
        let scorer = Scorer::new(19, 31);
        assert_eq!(scorer.window_log(1024), 10);
        assert_eq!(scorer.window_log(1 << 20), 20);
        assert_eq!(scorer.window_log((1 << 20) + 1), 21);
        assert_eq!(scorer.window_log(u64::MAX), 31);
    }
}
