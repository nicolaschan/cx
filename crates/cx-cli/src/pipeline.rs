//! Orchestrates a scoring run: resolve refs, fetch blobs, filter, build
//! the two references, run the three metric passes, rescale.

use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use cx_core::{Scorer, rescale};
use serde::Serialize;

use crate::filter::Filter;
use crate::git::{Git, Status};

const DEFAULT_BASES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

#[derive(Default)]
pub struct DiffOptions {
    pub base: Option<String>,
    pub staged: bool,
    /// Exclude test files from the universe entirely — they are then in
    /// no reference and no scoring pass, and appear as skipped.
    pub ignore_tests: bool,
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
    /// Lines added and deleted over the scored files: the familiar size
    /// beside the one this tool exists to report. Skipped files are not
    /// in it, so it covers what the bytes above it cover.
    pub added_lines: u64,
    pub deleted_lines: u64,
}

/// Lines by newline count: one measure wherever lines are reported.
fn lines(content: &[u8]) -> u64 {
    content.iter().filter(|&&b| b == b'\n').count() as u64
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
    /// Exclude test files from the universe entirely.
    pub ignore_tests: bool,
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
    pub rev: String,
    pub file_count: usize,
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    /// Empty when contributions were suppressed.
    pub files: Vec<AbsFile>,
    /// Attribution noise gauge for `files`; 1.0 when suppressed.
    pub scale: f64,
}

/// One changed file with whichever sides exist and passed the filter.
struct Item {
    path: String,
    status: Status,
    old: Option<Vec<u8>>,
    new: Option<Vec<u8>>,
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
    let changes = git.changes(&merge_base, opts.staged)?;

    // Fetch the whole old tree plus the new side of every change.
    let tree_paths = git.ls_tree(&merge_base)?;
    let tree_specs: Vec<String> = tree_paths
        .iter()
        .map(|p| format!("{merge_base}:{p}"))
        .collect();
    let old_tree: HashMap<&str, Vec<u8>> = tree_paths
        .iter()
        .map(String::as_str)
        .zip(git.blobs(&tree_specs)?)
        .filter_map(|(p, b)| Some((p, b?)))
        .collect();

    let new_side_paths: Vec<&str> = changes
        .iter()
        .filter(|c| c.status != Status::Deleted)
        .map(|c| c.path.as_str())
        .collect();
    let new_specs: Vec<String> = new_side_paths
        .iter()
        .map(|p| {
            if opts.staged {
                format!(":0:{p}")
            } else {
                format!("HEAD:{p}")
            }
        })
        .collect();
    let new_contents: HashMap<&str, Vec<u8>> = new_side_paths
        .iter()
        .copied()
        .zip(git.blobs(&new_specs)?)
        .filter_map(|(p, b)| Some((p, b?)))
        .collect();

    let attr_paths: Vec<String> = tree_paths
        .iter()
        .cloned()
        .chain(new_side_paths.iter().map(|p| p.to_string()))
        .collect();
    let filter = Filter::new(
        git.root(),
        git.linguist_attrs(&attr_paths)?,
        opts.ignore_tests,
    )?;

    // The universe is kept files only: a file the filter excludes exists
    // in no reference and no scoring pass.
    let kept_tree: Vec<&str> = tree_paths
        .iter()
        .map(String::as_str)
        .filter(|p| {
            old_tree
                .get(*p)
                .is_some_and(|c| filter.exclusion(p, c).is_none())
        })
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
        let old = old_path.and_then(|p| old_tree.get(p)).cloned();
        let new = (change.status != Status::Deleted)
            .then(|| new_contents.get(change.path.as_str()).cloned())
            .flatten();
        let exclusion = [
            (&new, change.path.as_str()),
            (&old, old_path.unwrap_or_default()),
        ]
        .into_iter()
        .find_map(|(content, path)| content.as_ref().and_then(|c| filter.exclusion(path, c)));
        if let Some(reason) = exclusion {
            skipped.push(Skipped {
                path: change.path.clone(),
                reason: reason.to_owned(),
            });
            continue;
        }
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
    let assemble = |paths: &[&str]| -> Vec<u8> {
        scorer.assemble(
            &paths
                .iter()
                .map(|p| old_tree[*p].as_slice())
                .collect::<Vec<_>>(),
        )
    };
    let old_tree_ref = assemble(&kept_tree);
    let remainder: Vec<&str> = kept_tree
        .iter()
        .copied()
        .filter(|p| !touched.contains(p))
        .collect();
    let remainder_ref = assemble(&remainder);

    // The three passes (plan §3): metric 1 against the full old tree,
    // metric 2 as new-vs-old against the neutral remainder, metric 3 as
    // joint compressions that are also the rescale targets.
    let new_items: Vec<&[u8]> = items.iter().filter_map(|i| i.new.as_deref()).collect();
    let old_items: Vec<&[u8]> = items.iter().filter_map(|i| i.old.as_deref()).collect();

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
        let new_lines = item.new.as_deref().map_or(0, lines);
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

    // Churn over the same files the bytes above cover: a skipped file is
    // absent from `items` and so absent from this too.
    let churn = git.line_counts(&merge_base, opts.staged)?;
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
    let rev = "HEAD".to_owned();
    let paths = git.ls_tree(&rev)?;
    let specs: Vec<String> = paths.iter().map(|p| format!("{rev}:{p}")).collect();
    let contents: Vec<(String, Vec<u8>)> = paths
        .into_iter()
        .zip(git.blobs(&specs)?)
        .filter_map(|(p, b)| Some((p, b?)))
        .collect();
    let attr_paths: Vec<String> = contents.iter().map(|(p, _)| p.clone()).collect();
    let filter = Filter::new(
        git.root(),
        git.linguist_attrs(&attr_paths)?,
        opts.ignore_tests,
    )?;
    let kept: Vec<(&String, &[u8])> = contents
        .iter()
        .filter(|(p, c)| filter.exclusion(p, c).is_none())
        .map(|(p, c)| (p, c.as_slice()))
        .collect();
    let kept_contents: Vec<&[u8]> = kept.iter().map(|(_, c)| *c).collect();

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
                path: (*path).clone(),
                bytes: score.rescaled,
                lines: lines(content),
            })
            .collect();
        (files, rescaled.scale)
    };
    files.sort_by(|a: &AbsFile, b: &AbsFile| b.bytes.total_cmp(&a.bytes));

    Ok(AbsReport {
        version: VersionInfo::for_scorer(&scorer),
        rev,
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

/// Layer 5 of the filter stack: flag (never drop) files whose density is
/// far off this run's median — probable generated/vendored content that
/// no pattern anticipated.
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
