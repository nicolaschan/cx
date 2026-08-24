//! Pure scoring engine: marginal description length of byte strings,
//! estimated by zstd compression conditioned on a reference.
//!
//! No git, no filesystem, no I/O — callers supply bytes. This is the
//! WASM-safe boundary; everything here is a pure function of its inputs.
//!
//! Scores are comparable only within one compressor version + parameter
//! set. Callers should surface [`zstd_version`] next to any scores.

pub mod testgen;

use zstd_safe::zstd_sys::ZSTD_EndDirective::ZSTD_e_flush;
use zstd_safe::{CCtx, CParameter, InBuffer, OutBuffer, compress_bound};

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

    /// C(input | reference): the bytes `input` adds to the compressed
    /// stream after `reference`.
    pub fn score(&self, reference: &[&[u8]], input: &[u8]) -> u64 {
        self.attribution(reference, &[input]).run(&|_| {})[0]
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

    /// Smallest window covering the whole stream, clamped to zstd's floor
    /// of 10 and this scorer's platform ceiling.
    fn window_log(&self, total_len: u64) -> u32 {
        let needed = u64::BITS - total_len.saturating_sub(1).leading_zeros();
        needed.clamp(10, self.max_window_log)
    }
}

/// One compressed stream: the reference, then each item, with a flush at
/// every part boundary. An item's score is the bytes its part added to
/// the output — C(item_i | reference ++ items[..i]), the chain rule, so a
/// pattern repeated across items is charged to its first occurrence and
/// near-free afterwards. Nothing after a part can change its score.
pub struct Attribution<'a> {
    scorer: &'a Scorer,
    reference: &'a [&'a [u8]],
    items: &'a [&'a [u8]],
}

impl Attribution<'_> {
    fn parts(&self) -> impl Iterator<Item = &[u8]> {
        self.reference.iter().chain(self.items).copied()
    }

    /// Bytes to compress: the unit progress advances in.
    pub fn cost(&self) -> u64 {
        self.parts().map(|part| part.len() as u64).sum()
    }

    /// Each item's score, in order. `progress` receives each part's
    /// length as it finishes, reference parts included.
    pub fn run(&self, progress: &(dyn Fn(u64) + Sync)) -> Vec<u64> {
        let mut cctx = CCtx::create();
        let set = |cctx: &mut CCtx, p| {
            cctx.set_parameter(p).expect("static zstd parameter");
        };
        set(&mut cctx, CParameter::CompressionLevel(self.scorer.level));
        // Determinism by construction, not by libzstd's default: MT zstd
        // changes output sizes.
        set(&mut cctx, CParameter::NbWorkers(0));
        set(&mut cctx, CParameter::EnableLongDistanceMatching(true));
        set(
            &mut cctx,
            CParameter::WindowLog(self.scorer.window_log(self.cost())),
        );

        let bound: usize = self.parts().map(|part| compress_bound(part.len())).sum();
        let mut out = Vec::with_capacity(bound + compress_bound(0));
        let mut sink = OutBuffer::around(&mut out);
        let mut feed = |part: &[u8]| -> u64 {
            let before = sink.pos();
            let mut source = InBuffer::around(part);
            loop {
                let pending = cctx
                    .compress_stream2(&mut sink, &mut source, ZSTD_e_flush)
                    .unwrap_or_else(|c| panic!("zstd: {}", zstd_safe::get_error_name(c)));
                if pending == 0 && source.pos() == part.len() {
                    break;
                }
            }
            progress(part.len() as u64);
            (sink.pos() - before) as u64
        };
        for part in self.reference {
            feed(part);
        }
        self.items.iter().map(|item| feed(item)).collect()
    }
}

/// Run several attributions at once, one thread each. Results land in
/// the same order as `attributions`.
pub fn run_all<const N: usize>(
    attributions: [&Attribution<'_>; N],
    progress: &(dyn Fn(u64) + Sync),
) -> [Vec<u64>; N] {
    std::thread::scope(|scope| {
        attributions
            .map(|attribution| scope.spawn(move || attribution.run(progress)))
            .map(|stream| stream.join().expect("stream thread"))
    })
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
