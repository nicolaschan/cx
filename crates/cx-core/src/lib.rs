//! Pure scoring engine: marginal description length of byte strings,
//! estimated by zstd compression against a reference prefix.
//!
//! No git, no filesystem, no I/O — callers supply bytes. This is the
//! WASM-safe boundary; everything here is a pure function of its inputs.
//!
//! Scores are comparable only within one compressor version + parameter
//! set. Callers should surface [`zstd_version`] next to any scores.

use zstd_safe::{CCtx, CParameter, compress_bound};

/// Separator inserted between files when assembling references, chosen to
/// be improbable in real content so zstd matches can't span file
/// boundaries spuriously.
pub const SEPARATOR: &[u8] = b"\0CX-SEP\0CX-SEP\0";

/// The zstd library version this build scores with, e.g. "1.5.6".
pub fn zstd_version() -> String {
    let n = zstd_safe::version_number();
    format!("{}.{}.{}", n / 10000, (n / 100) % 100, n % 100)
}

/// A sequentially-attributed score: the raw compressed size, and the same
/// score rescaled so that all items in a run sum to their joint total.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SeqScore {
    pub raw: u64,
    pub rescaled: f64,
}

/// Result of [`rescale`]: per-item scores plus the scale factor, which
/// doubles as a noise gauge (≈ 1.0 → trust per-item attribution).
#[derive(Clone, Debug)]
pub struct Rescaled {
    pub scores: Vec<SeqScore>,
    pub scale: f64,
}

/// Rescale raw sequential scores to sum to the joint total. Degenerate
/// case: when every raw score is 0 (e.g. all pure moves) there is nothing
/// to attribute, so rescaled scores stay 0 and scale reads 1.0 even if
/// the joint is a few overhead bytes — the sum-to-joint property holds
/// only when something scored.
pub fn rescale(raw: &[u64], joint: u64) -> Rescaled {
    let sum: u64 = raw.iter().sum();
    let scale = if sum == 0 {
        1.0
    } else {
        joint as f64 / sum as f64
    };
    Rescaled {
        scores: raw
            .iter()
            .map(|&r| SeqScore {
                raw: r,
                rescaled: r as f64 * scale,
            })
            .collect(),
        scale,
    }
}

pub struct Scorer {
    level: i32,
    max_window_log: u32,
    /// Compressed size of the empty input under the same parameters:
    /// pure frame overhead, subtracted from every score.
    empty_frame: u64,
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
        let mut scorer = Scorer {
            level,
            max_window_log,
            empty_frame: 0,
        };
        scorer.empty_frame = scorer.compressed_size(&[], &[]);
        scorer
    }

    /// Join parts with [`SEPARATOR`]. Ordering policy belongs to the
    /// caller: pass parts already in the order they should appear.
    pub fn assemble(&self, parts: &[&[u8]]) -> Vec<u8> {
        let total: usize = parts.iter().map(|p| p.len() + SEPARATOR.len()).sum();
        let mut out = Vec::with_capacity(total);
        for part in parts {
            out.extend_from_slice(part);
            out.extend_from_slice(SEPARATOR);
        }
        out
    }

    /// C(item_i | reference ++ items[..i]) for each item: sequential
    /// chain-rule scoring, so a pattern repeated across items is charged
    /// to its first occurrence and near-free afterwards.
    pub fn score_sequential(&self, reference: &[u8], items: &[&[u8]]) -> Vec<u64> {
        let grown: usize = items.iter().map(|i| i.len() + SEPARATOR.len()).sum();
        let mut prefix = Vec::with_capacity(reference.len() + grown);
        prefix.extend_from_slice(reference);
        items
            .iter()
            .map(|item| {
                let size = self.score(&prefix, item);
                prefix.extend_from_slice(item);
                prefix.extend_from_slice(SEPARATOR);
                size
            })
            .collect()
    }

    /// C(all items jointly | reference): one compression of the
    /// separator-joined items. The rescale target for sequential runs.
    pub fn score_joint(&self, reference: &[u8], items: &[&[u8]]) -> u64 {
        self.score(reference, &self.assemble(items))
    }

    /// Plain C(blob), no reference. The absolute-trend metric.
    pub fn score_absolute(&self, blob: &[u8]) -> u64 {
        self.score(&[], blob)
    }

    fn score(&self, prefix: &[u8], input: &[u8]) -> u64 {
        self.compressed_size(prefix, input)
            .saturating_sub(self.empty_frame)
    }

    fn compressed_size(&self, prefix: &[u8], input: &[u8]) -> u64 {
        let mut cctx = CCtx::create();
        let set = |cctx: &mut CCtx, p| {
            cctx.set_parameter(p).expect("static zstd parameter");
        };
        set(&mut cctx, CParameter::CompressionLevel(self.level));
        // Determinism by construction, not by libzstd's default: MT zstd
        // changes output sizes.
        set(&mut cctx, CParameter::NbWorkers(0));
        set(&mut cctx, CParameter::EnableLongDistanceMatching(true));
        set(
            &mut cctx,
            CParameter::WindowLog(self.window_log(prefix.len() + input.len())),
        );
        // Uniform frame overhead: no stored content size, no checksum,
        // so the empty-frame constant holds for every input size.
        set(&mut cctx, CParameter::ContentSizeFlag(false));
        set(&mut cctx, CParameter::ChecksumFlag(false));
        if !prefix.is_empty() {
            cctx.ref_prefix(prefix)
                .expect("ref_prefix accepts any bytes");
        }
        let mut out: Vec<u8> = Vec::with_capacity(compress_bound(input.len()));
        let written = cctx
            .compress2(&mut out, input)
            .unwrap_or_else(|c| panic!("zstd compress2: {}", zstd_safe::get_error_name(c)));
        written as u64
    }

    /// Smallest window covering the whole reference + input, clamped to
    /// zstd's floor of 10 and this scorer's platform ceiling.
    fn window_log(&self, total_len: usize) -> u32 {
        let needed = usize::BITS - total_len.saturating_sub(1).leading_zeros();
        needed.clamp(10, self.max_window_log)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_scores_zero() {
        let scorer = Scorer::default();
        assert_eq!(scorer.score_absolute(&[]), 0);
    }

    #[test]
    fn window_log_covers_reference() {
        let scorer = Scorer::new(19, 31);
        assert_eq!(scorer.window_log(1024), 10);
        assert_eq!(scorer.window_log(1 << 20), 20);
        assert_eq!(scorer.window_log((1 << 20) + 1), 21);
        assert_eq!(scorer.window_log(usize::MAX), 31);
    }
}
