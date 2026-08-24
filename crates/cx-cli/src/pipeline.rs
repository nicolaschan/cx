//! Orchestrates a scoring run: resolve refs, fetch blobs, filter, build
//! the two references, run the three metric passes, rescale.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use cx_core::{Scorer, rescale};
use serde::Serialize;

use crate::filter::Filter;
use crate::git::{Git, Side, Status};
use crate::strip;

const DEFAULT_BASES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

/// What is scored: which snapshot, and which kinds of content count.
#[derive(Clone, Copy, Default)]
pub struct Scope {
    pub side: Side,
    /// Exclude test files from the universe entirely — they are then in
    /// no reference and no scoring pass, and appear as skipped.
    pub ignore_tests: bool,
    /// Score comments too. Otherwise every blob is reduced to code before
    /// it enters any reference or scoring pass.
    pub comments: bool,
    /// Score prose files (Markdown, reStructuredText, …) too. Otherwise
    /// they appear as skipped.
    pub prose: bool,
}

#[derive(Default)]
pub struct DiffOptions {
    pub base: Option<String>,
    pub scope: Scope,
}

#[derive(Serialize)]
pub struct VersionInfo {
    pub cx: &'static str,
    pub zstd: String,
    pub level: i32,
    pub max_window_log: u32,
}

impl VersionInfo {
    /// Provenance from the scorer that actually produced the numbers —
    /// never restated by hand.
    fn for_scorer(scorer: &Scorer) -> Self {
        VersionInfo {
            cx: env!("CARGO_PKG_VERSION"),
            zstd: cx_core::zstd_version(),
            level: scorer.level(),
            max_window_log: scorer.max_window_log(),
        }
    }
}

#[derive(Serialize)]
pub struct DiffFile {
    pub path: String,
    /// Serializes as "added" | "modified" | "deleted" |
    /// "renamed from <path>".
    #[serde(serialize_with = "serialize_status")]
    pub status: Status,
    /// Metric 1, rescaled: what the reviewer must newly absorb.
    pub review_bytes: f64,
    pub review_raw: u64,
    /// Metric 2, rescaled: complexity added (+) or removed (−).
    pub delta_bytes: f64,
    pub new_lines: u64,
    /// review_bytes / new_lines — density separates tables from
    /// algorithms. Only defined for added files, where all lines are
    /// new; for a modified file the marginal bytes over the file's total
    /// lines would measure nothing.
    pub bytes_per_line: Option<f64>,
    /// Set when bytes_per_line is a >5× or <0.1× outlier vs the run
    /// median: probable generated/vendored content no pattern caught.
    pub density_outlier: bool,
}

#[derive(Serialize)]
pub struct Skipped {
    pub path: String,
    pub reason: String,
}

#[derive(Serialize)]
pub struct Totals {
    pub review_bytes: u64,
    pub delta_bytes: i64,
    pub added_lines: u64,
    pub deleted_lines: u64,
}

fn lines(content: &[u8]) -> u64 {
    content.iter().filter(|&&b| b == b'\n').count() as u64
}

/// What a blob becomes on its way into scoring: its code, or the reason
/// the filter dropped it. The filter sees raw bytes (binary detection
/// needs them); everything after sees code only.
type Prepared = Result<Vec<u8>, &'static str>;

fn prepare(filter: &Filter, scope: &Scope, path: &str, raw: Vec<u8>) -> Prepared {
    match filter.exclusion(path, &raw) {
        Some(reason) => Err(reason),
        None if scope.comments => Ok(raw),
        None => Ok(strip::code_only(path, raw)),
    }
}

/// Every path with a blob, prepared, in the order `paths` was given.
/// Paths whose blob is missing (a submodule, a file gone from disk) are
/// simply absent.
fn load<'a>(
    filter: &Filter,
    scope: &Scope,
    paths: &[&'a str],
    blobs: Vec<Option<Vec<u8>>>,
) -> Vec<(&'a str, Prepared)> {
    paths
        .iter()
        .copied()
        .zip(blobs)
        .filter_map(|(path, blob)| Some((path, prepare(filter, scope, path, blob?))))
        .collect()
}

/// The code of a prepared blob, or None if it was dropped (or absent).
fn code(prepared: Option<&Prepared>) -> Option<&[u8]> {
    prepared?.as_deref().ok()
}

#[derive(Serialize)]
pub struct Scales {
    pub review: f64,
    pub delta_new: f64,
    pub delta_old: f64,
}

#[derive(Serialize)]
pub struct DiffReport {
    pub version: VersionInfo,
    pub base: String,
    pub merge_base: String,
    pub files: Vec<DiffFile>,
    pub skipped: Vec<Skipped>,
    pub totals: Totals,
    pub scales: Scales,
}

#[derive(Default)]
pub struct AbsOptions {
    /// Skip per-file contributions, leaving one joint compression —
    /// much faster on big trees.
    pub no_files: bool,
    pub scope: Scope,
}

/// One file's contribution to C(tree): its sequential chain-rule score in
/// sorted-path order, rescaled so contributions sum to the joint total.
#[derive(Serialize)]
pub struct AbsFile {
    pub path: String,
    pub bytes: f64,
    pub lines: u64,
}

#[derive(Serialize)]
pub struct AbsReport {
    pub version: VersionInfo,
    pub snapshot: &'static str,
    pub file_count: usize,
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    /// Empty when contributions were suppressed.
    pub files: Vec<AbsFile>,
    /// Attribution noise gauge for `files`; 1.0 when suppressed.
    pub scale: f64,
}

/// One changed file with whichever sides exist and passed the filter.
struct Item<'a> {
    path: String,
    status: Status,
    old: Option<&'a [u8]>,
    new: Option<&'a [u8]>,
}

/// The status stays typed everywhere; this string form exists only at
/// the JSON edge.
fn serialize_status<S: serde::Serializer>(status: &Status, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&match status {
        Status::Added => "added".to_owned(),
        Status::Modified => "modified".to_owned(),
        Status::Deleted => "deleted".to_owned(),
        Status::Renamed { from } => format!("renamed from {from}"),
    })
}

pub fn diff(git: &Git, opts: &DiffOptions) -> Result<DiffReport> {
    let base = resolve_base(git, opts.base.as_deref())?;
    let merge_base = git.merge_base(&base, "HEAD")?;
    let changes = git.changes(&merge_base, opts.scope.side)?;

    let scope = &opts.scope;
    let tree_paths = git.ls_tree(&merge_base)?;
    let tree_refs: Vec<&str> = tree_paths.iter().map(String::as_str).collect();
    let new_side_paths: Vec<&str> = changes
        .iter()
        .filter(|c| c.status != Status::Deleted)
        .map(|c| c.path.as_str())
        .collect();
    let attr_paths: Vec<String> = tree_paths
        .iter()
        .cloned()
        .chain(new_side_paths.iter().map(|p| p.to_string()))
        .collect();
    let filter = Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, scope)?;

    // The whole old tree plus the new side of every change, each blob
    // filtered and reduced to code once.
    let old_tree: HashMap<&str, Prepared> = load(
        &filter,
        scope,
        &tree_refs,
        git.tree_contents(&merge_base, &tree_refs)?,
    )
    .into_iter()
    .collect();
    let new_contents: HashMap<&str, Prepared> = load(
        &filter,
        scope,
        &new_side_paths,
        git.contents(scope.side, &new_side_paths)?,
    )
    .into_iter()
    .collect();

    // The universe is kept files only: a file the filter excludes exists
    // in no reference and no scoring pass.
    let kept_tree: Vec<(&str, &[u8])> = tree_refs
        .iter()
        .filter_map(|p| Some((*p, code(old_tree.get(p))?)))
        .collect();

    // Partition changes into scorable items and skipped files. A change
    // is skipped when any side it has fails the filter (e.g. a file that
    // flipped binary→text is skipped whole rather than half-scored).
    let mut items: Vec<Item> = Vec::new();
    let mut skipped: Vec<Skipped> = Vec::new();
    let mut touched: HashSet<&str> = HashSet::new();
    for change in &changes {
        touched.insert(change.path.as_str());
        let old_path = match &change.status {
            Status::Added => None,
            Status::Modified | Status::Deleted => Some(change.path.as_str()),
            Status::Renamed { from } => {
                touched.insert(from.as_str());
                Some(from.as_str())
            }
        };
        let old = old_path.and_then(|p| old_tree.get(p));
        let new = (change.status != Status::Deleted)
            .then(|| new_contents.get(change.path.as_str()))
            .flatten();
        if let Some(reason) = [new, old]
            .into_iter()
            .flatten()
            .find_map(|b| b.as_ref().err())
        {
            skipped.push(Skipped {
                path: change.path.clone(),
                reason: (*reason).to_owned(),
            });
            continue;
        }
        let (old, new) = (code(old), code(new));
        if old.is_none() && new.is_none() {
            continue;
        }
        items.push(Item {
            path: change.path.clone(),
            status: change.status.clone(),
            old,
            new,
        });
    }
    items.sort_by(|a, b| a.path.cmp(&b.path));

    let scorer = Scorer::default();
    let old_tree_ref = scorer.assemble(&kept_tree.iter().map(|(_, c)| *c).collect::<Vec<_>>());
    let remainder: Vec<&[u8]> = kept_tree
        .iter()
        .filter(|(p, _)| !touched.contains(p))
        .map(|(_, c)| *c)
        .collect();
    let remainder_ref = scorer.assemble(&remainder);

    // The three passes (plan §3): metric 1 against the full old tree,
    // metric 2 as new-vs-old against the neutral remainder, metric 3 as
    // joint compressions that are also the rescale targets.
    let new_items: Vec<&[u8]> = items.iter().filter_map(|i| i.new).collect();
    let old_items: Vec<&[u8]> = items.iter().filter_map(|i| i.old).collect();

    let review_seq = scorer.score_sequential(&old_tree_ref, &new_items);
    let review_joint = scorer.score_joint(&old_tree_ref, &new_items);
    let review = rescale(&review_seq, review_joint);

    let delta_new_seq = scorer.score_sequential(&remainder_ref, &new_items);
    let delta_new_joint = scorer.score_joint(&remainder_ref, &new_items);
    let delta_new = rescale(&delta_new_seq, delta_new_joint);

    let delta_old_seq = scorer.score_sequential(&remainder_ref, &old_items);
    let delta_old_joint = scorer.score_joint(&remainder_ref, &old_items);
    let delta_old = rescale(&delta_old_seq, delta_old_joint);

    let mut files = Vec::with_capacity(items.len());
    let (mut new_i, mut old_i) = (0, 0);
    for item in &items {
        let (review_bytes, review_raw, new_delta) = if item.new.is_some() {
            let r = (review.scores[new_i], delta_new.scores[new_i]);
            new_i += 1;
            (r.0.rescaled, r.0.raw, r.1.rescaled)
        } else {
            (0.0, 0, 0.0)
        };
        let old_delta = if item.old.is_some() {
            let d = delta_old.scores[old_i].rescaled;
            old_i += 1;
            d
        } else {
            0.0
        };
        let new_lines = item.new.map_or(0, lines);
        files.push(DiffFile {
            path: item.path.clone(),
            status: item.status.clone(),
            review_bytes,
            review_raw,
            delta_bytes: new_delta - old_delta,
            new_lines,
            bytes_per_line: (item.status == Status::Added && new_lines > 0)
                .then(|| review_bytes / new_lines as f64),
            density_outlier: false,
        });
    }
    flag_density_outliers(&mut files);
    files.sort_by(|a, b| b.review_bytes.total_cmp(&a.review_bytes));

    let churn = git.line_counts(&merge_base, opts.scope.side)?;
    let (added_lines, deleted_lines) = items
        .iter()
        .filter_map(|item| churn.get(&item.path))
        .fold((0, 0), |(a, d), (added, deleted)| (a + added, d + deleted));

    Ok(DiffReport {
        version: VersionInfo::for_scorer(&scorer),
        base,
        merge_base,
        files,
        skipped,
        totals: Totals {
            review_bytes: review_joint,
            delta_bytes: delta_new_joint as i64 - delta_old_joint as i64,
            added_lines,
            deleted_lines,
        },
        scales: Scales {
            review: review.scale,
            delta_new: delta_new.scale,
            delta_old: delta_old.scale,
        },
    })
}

pub fn abs(git: &Git, opts: &AbsOptions) -> Result<AbsReport> {
    let scope = &opts.scope;
    let paths = git.list(scope.side)?;
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let blobs = git.contents(scope.side, &path_refs)?;
    let attr_paths: Vec<String> = path_refs
        .iter()
        .zip(&blobs)
        .filter_map(|(p, b)| b.as_ref().map(|_| p.to_string()))
        .collect();
    let filter = Filter::new(git.root(), git.linguist_attrs(&attr_paths)?, scope)?;
    let kept: Vec<(&str, Vec<u8>)> = load(&filter, scope, &path_refs, blobs)
        .into_iter()
        .filter_map(|(p, prepared)| Some((p, prepared.ok()?)))
        .collect();
    let kept_contents: Vec<&[u8]> = kept.iter().map(|(_, c)| c.as_slice()).collect();

    let scorer = Scorer::default();
    let blob = scorer.assemble(&kept_contents);
    let compressed = scorer.score_absolute(&blob);

    // Per-file contribution: the chain rule over sorted paths against an
    // empty reference — the same attribution machinery as diff scoring,
    // with C(tree) itself as the rescale target.
    let (mut files, scale) = if opts.no_files {
        (Vec::new(), 1.0)
    } else {
        let rescaled = rescale(&scorer.score_sequential(&[], &kept_contents), compressed);
        let files = kept
            .iter()
            .zip(&rescaled.scores)
            .map(|((path, content), score)| AbsFile {
                path: (*path).to_owned(),
                bytes: score.rescaled,
                lines: lines(content),
            })
            .collect();
        (files, rescaled.scale)
    };
    files.sort_by(|a: &AbsFile, b: &AbsFile| b.bytes.total_cmp(&a.bytes));

    Ok(AbsReport {
        version: VersionInfo::for_scorer(&scorer),
        snapshot: opts.scope.side.label(),
        file_count: kept.len(),
        raw_bytes: blob.len() as u64,
        compressed_bytes: compressed,
        files,
        scale,
    })
}

fn resolve_base(git: &Git, requested: Option<&str>) -> Result<String> {
    if let Some(base) = requested {
        if git.resolve(base)?.is_none() {
            bail!("--base {base} does not resolve to a commit");
        }
        return Ok(base.to_owned());
    }
    for candidate in DEFAULT_BASES {
        if git.resolve(candidate)?.is_some() {
            return Ok(candidate.to_owned());
        }
    }
    bail!("no main/master branch found; pass --base <ref>")
}

/// The density backstop of the filter stack (see `filter.rs`): flag
/// (never drop) files whose density is far off this run's median —
/// probable generated/vendored content that no pattern anticipated.
fn flag_density_outliers(files: &mut [DiffFile]) {
    let mut densities: Vec<f64> = files
        .iter()
        .filter_map(|f| f.bytes_per_line)
        .filter(|&d| d > 0.0)
        .collect();
    if densities.len() < 3 {
        return;
    }
    densities.sort_by(f64::total_cmp);
    let median = densities[densities.len() / 2];
    for file in files {
        if let Some(d) = file.bytes_per_line {
            file.density_outlier = d > 5.0 * median || (d > 0.0 && d < 0.1 * median);
        }
    }
}
