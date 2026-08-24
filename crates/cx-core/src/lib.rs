//! Pure scoring engine: marginal description length of byte strings,
//! estimated by zstd compression against a reference prefix.
//!
//! No git, no filesystem, no I/O — callers supply bytes. This is the
//! WASM-safe boundary; everything here is a pure function of its inputs.
//!
//! Scores are comparable only within one compressor version + parameter
//! set. Callers should surface [`zstd_version`] next to any scores.

pub mod testgen;

use std::cmp::Reverse;
use std::ops::Range;
use std::sync::Mutex;

use rayon::prelude::*;
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

/// Result of [`rescale`]: per-item scores, the joint they sum to, and the
/// scale factor, which doubles as a noise gauge (≈ 1.0 → trust per-item
/// attribution).
#[derive(Clone, Debug)]
pub struct Rescaled {
    pub scores: Vec<SeqScore>,
    pub scale: f64,
    pub joint: u64,
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
        joint,
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
        // A scorer that subtracts nothing measures the overhead itself.
        let raw = Scorer {
            level,
            max_window_log,
            empty_frame: 0,
        };
        Scorer {
            empty_frame: raw.score(&[], &[]),
            ..raw
        }
    }

    /// Join parts with [`SEPARATOR`]. Ordering policy belongs to the
    /// caller: pass parts already in the order they should appear.
    pub fn assemble(&self, parts: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        append(&mut out, parts);
        out
    }

    /// C(input | reference): one compression.
    pub fn score(&self, reference: &[u8], input: &[u8]) -> u64 {
        self.compress(&mut CCtx::create(), reference, input)
    }

    pub fn attribution<'s>(&'s self, reference: &[u8], items: &[&[u8]]) -> Attribution<'s> {
        let mut buffer = reference.to_vec();
        let items = append(&mut buffer, items);
        Attribution {
            scorer: self,
            joint: reference.len()..buffer.len(),
            buffer,
            items,
        }
    }

    fn compress<'a>(&self, cctx: &mut CCtx<'a>, prefix: &'a [u8], input: &[u8]) -> u64 {
        let set = |cctx: &mut CCtx, p| {
            cctx.set_parameter(p).expect("static zstd parameter");
        };
        set(cctx, CParameter::CompressionLevel(self.level));
        // Determinism by construction, not by libzstd's default: MT zstd
        // changes output sizes.
        set(cctx, CParameter::NbWorkers(0));
        // So does a prefix adjacent to its input in memory.
        set(cctx, CParameter::DeterministicRefPrefix(true));
        set(cctx, CParameter::EnableLongDistanceMatching(true));
        set(
            cctx,
            CParameter::WindowLog(self.window_log(prefix.len() + input.len())),
        );
        // Uniform frame overhead: no stored content size, no checksum,
        // so the empty-frame constant holds for every input size.
        set(cctx, CParameter::ContentSizeFlag(false));
        set(cctx, CParameter::ChecksumFlag(false));
        if !prefix.is_empty() {
            cctx.ref_prefix(prefix)
                .expect("ref_prefix accepts any bytes");
        }
        let mut out: Vec<u8> = Vec::with_capacity(compress_bound(input.len()));
        let written = cctx
            .compress2(&mut out, input)
            .unwrap_or_else(|c| panic!("zstd compress2: {}", zstd_safe::get_error_name(c)));
        (written as u64).saturating_sub(self.empty_frame)
    }

    /// Smallest window covering the whole reference + input, clamped to
    /// zstd's floor of 10 and this scorer's platform ceiling.
    fn window_log(&self, total_len: usize) -> u32 {
        let needed = usize::BITS - total_len.saturating_sub(1).leading_zeros();
        needed.clamp(10, self.max_window_log)
    }
}

/// Append each part followed by [`SEPARATOR`], returning where each part
/// landed.
fn append(out: &mut Vec<u8>, parts: &[&[u8]]) -> Vec<Range<usize>> {
    out.reserve(parts.iter().map(|p| p.len() + SEPARATOR.len()).sum());
    parts
        .iter()
        .map(|part| {
            let start = out.len();
            out.extend_from_slice(part);
            out.extend_from_slice(SEPARATOR);
            start..start + part.len()
        })
        .collect()
}

/// The independent compressions behind attributing `items` to
/// `reference`, over one buffer `reference ++ item₀ ++ SEP ++ item₁ …`:
/// C(item_i | reference ++ items[..i]) for each item — the chain rule,
/// so a pattern repeated across items is charged to its first
/// occurrence and near-free afterwards — and C(all items jointly |
/// reference), the rescale target. Every input is scored against
/// everything before it in the buffer.
pub struct Attribution<'s> {
    scorer: &'s Scorer,
    buffer: Vec<u8>,
    items: Vec<Range<usize>>,
    joint: Range<usize>,
}

/// Bytes zstd indexes for an input: its prefix plus itself.
fn indexed(input: &Range<usize>) -> u64 {
    input.end as u64
}

impl Attribution<'_> {
    fn inputs(&self) -> impl Iterator<Item = &Range<usize>> {
        self.items.iter().chain([&self.joint])
    }

    /// Bytes zstd indexes over every input: the unit progress advances in.
    pub fn cost(&self) -> u64 {
        self.inputs().map(indexed).sum()
    }

    pub fn run(&self, progress: &(dyn Fn(u64) + Sync)) -> Rescaled {
        let [scored] = run_all([self], progress);
        scored
    }
}

/// Every compression of every attribution as one parallel batch: longest
/// job first so the tail stays short, zstd contexts pooled rather than
/// created per job. `progress` receives each compression's
/// cost as it finishes.
pub fn run_all<const N: usize>(
    attributions: [&Attribution<'_>; N],
    progress: &(dyn Fn(u64) + Sync),
) -> [Rescaled; N] {
    let mut slots = attributions.map(|a| (vec![0; a.items.len()], 0));
    let mut jobs: Vec<_> = attributions
        .iter()
        .zip(&mut slots)
        .flat_map(|(&a, (items, joint))| {
            a.inputs()
                .zip(items.iter_mut().chain([joint]))
                .map(move |(input, slot)| (a, input, slot))
        })
        .collect();
    jobs.sort_by_key(|(_, input, _)| Reverse(indexed(input)));

    let contexts = Mutex::new(Vec::new());
    jobs.into_par_iter()
        .with_max_len(1)
        .for_each(|(a, input, slot)| {
            let mut cctx = contexts
                .lock()
                .expect("pool")
                .pop()
                .unwrap_or_else(CCtx::create);
            *slot = a.scorer.compress(
                &mut cctx,
                &a.buffer[..input.start],
                &a.buffer[input.clone()],
            );
            contexts.lock().expect("pool").push(cctx);
            progress(indexed(input));
        });

    slots.map(|(items, joint)| rescale(&items, joint))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_scores_zero() {
        let scorer = Scorer::default();
        assert_eq!(scorer.score(&[], &[]), 0);
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
