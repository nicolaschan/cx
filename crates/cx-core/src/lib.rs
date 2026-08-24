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

/// Result of an attribution: per-item scores, the joint they are rescaled
/// to sum to, and the scale factor, which doubles as a noise gauge
/// (≈ 1.0 → trust per-item attribution).
#[derive(Clone, Debug)]
pub struct Rescaled {
    pub scores: Vec<SeqScore>,
    pub scale: f64,
    /// C(all items | reference): the rescale target.
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

/// Receives each compression's cost as it finishes. Over one run the
/// calls sum to the run's [`Attribution::cost`].
pub trait Progress: Sync {
    fn advance(&self, bytes: u64);
}

/// No progress reporting.
pub struct Silent;

impl Progress for Silent {
    fn advance(&self, _: u64) {}
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
        scorer.empty_frame = scorer.compressed_size(&mut CCtx::create(), &[], &[]);
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

    /// C(input | reference): one compression, the primitive every score
    /// is built from. An empty reference gives the absolute C(input).
    pub fn score(&self, reference: &[u8], input: &[u8]) -> u64 {
        self.compress(&mut CCtx::create(), reference, input)
    }

    /// The compressions behind attributing `items` to `reference`, laid
    /// out and ready to [`run`](Attribution::run).
    pub fn attribution<'s>(&'s self, reference: &[u8], items: &[&[u8]]) -> Attribution<'s> {
        let grown: usize = items.iter().map(|i| i.len() + SEPARATOR.len()).sum();
        let mut buffer = Vec::with_capacity(reference.len() + grown);
        buffer.extend_from_slice(reference);
        let items = items
            .iter()
            .map(|item| {
                let start = buffer.len();
                buffer.extend_from_slice(item);
                buffer.extend_from_slice(SEPARATOR);
                start..start + item.len()
            })
            .collect();
        Attribution {
            scorer: self,
            buffer,
            reference_len: reference.len(),
            items,
        }
    }

    fn compress<'a>(&self, cctx: &mut CCtx<'a>, reference: &'a [u8], input: &[u8]) -> u64 {
        self.compressed_size(cctx, reference, input)
            .saturating_sub(self.empty_frame)
    }

    fn compressed_size<'a>(&self, cctx: &mut CCtx<'a>, prefix: &'a [u8], input: &[u8]) -> u64 {
        let set = |cctx: &mut CCtx, p| {
            cctx.set_parameter(p).expect("static zstd parameter");
        };
        set(cctx, CParameter::CompressionLevel(self.level));
        // Determinism by construction, not by libzstd's default: MT zstd
        // changes output sizes, and so does a prefix that happens to sit
        // adjacent to its input in memory.
        set(cctx, CParameter::NbWorkers(0));
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
        written as u64
    }

    /// Smallest window covering the whole reference + input, clamped to
    /// zstd's floor of 10 and this scorer's platform ceiling.
    fn window_log(&self, total_len: usize) -> u32 {
        let needed = usize::BITS - total_len.saturating_sub(1).leading_zeros();
        needed.clamp(10, self.max_window_log)
    }
}

/// One attribution's compressions, over a single buffer:
/// `reference ++ item₀ ++ SEP ++ item₁ ++ SEP …`. Item i is scored
/// against everything before it (the chain rule: a pattern repeated
/// across items is charged to its first occurrence), and all items
/// jointly against the reference. No compression reads another's
/// result, so they all run at once.
pub struct Attribution<'s> {
    scorer: &'s Scorer,
    buffer: Vec<u8>,
    reference_len: usize,
    items: Vec<Range<usize>>,
}

/// One compression: `buffer[input]` against `buffer[..prefix_end]`.
struct Job {
    prefix_end: usize,
    input: Range<usize>,
    slot: Slot,
}

enum Slot {
    Item(usize),
    Joint,
}

impl Job {
    /// Bytes zstd indexes and compresses.
    fn cost(&self) -> u64 {
        (self.prefix_end + self.input.len()) as u64
    }
}

impl Attribution<'_> {
    fn jobs(&self) -> impl Iterator<Item = Job> + '_ {
        let items = self.items.iter().enumerate().map(|(i, item)| Job {
            prefix_end: item.start,
            input: item.clone(),
            slot: Slot::Item(i),
        });
        items.chain([Job {
            prefix_end: self.reference_len,
            input: self.reference_len..self.buffer.len(),
            slot: Slot::Joint,
        }])
    }

    /// Total [`Job::cost`] over every job: the unit progress advances
    /// in, so a bar over it tracks wall-clock rather than item count.
    pub fn cost(&self) -> u64 {
        self.jobs().map(|job| job.cost()).sum()
    }

    pub fn run(&self, progress: &dyn Progress) -> Rescaled {
        let [scored] = run_all([self], progress);
        scored
    }
}

/// Run every compression of every attribution as one parallel batch:
/// longest job first so the tail stays short, one zstd context per
/// worker rather than a fresh one (and its ~80 MB of zeroed tables)
/// per job. Results land in the same order as `plans`.
pub fn run_all<const N: usize>(
    plans: [&Attribution<'_>; N],
    progress: &dyn Progress,
) -> [Rescaled; N] {
    let mut jobs: Vec<(usize, Job)> = plans
        .iter()
        .enumerate()
        .flat_map(|(p, plan)| plan.jobs().map(move |job| (p, job)))
        .collect();
    jobs.sort_by_key(|(_, job)| Reverse(job.cost()));

    let contexts = Mutex::new(Vec::new());
    let sizes: Vec<u64> = jobs
        .par_iter()
        .with_max_len(1)
        .map(|(p, job)| {
            let plan = plans[*p];
            let mut cctx = contexts
                .lock()
                .expect("pool")
                .pop()
                .unwrap_or_else(CCtx::create);
            let size = plan.scorer.compress(
                &mut cctx,
                &plan.buffer[..job.prefix_end],
                &plan.buffer[job.input.clone()],
            );
            contexts.lock().expect("pool").push(cctx);
            progress.advance(job.cost());
            size
        })
        .collect();

    let mut raw = plans.map(|plan| vec![0; plan.items.len()]);
    let mut joint = [0; N];
    for ((p, job), size) in jobs.iter().zip(sizes) {
        match job.slot {
            Slot::Item(i) => raw[*p][i] = size,
            Slot::Joint => joint[*p] = size,
        }
    }
    std::array::from_fn(|p| rescale(&raw[p], joint[p]))
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
