//! Orchestrates a scoring run: resolve refs, fetch blobs, filter, build
//! the two references, run the three metric passes.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use cx_core::{Scorer, run_all};
use serde::Serialize;

use crate::filter::Filter;
use crate::git::{Git, Side, Status};
use crate::progress::Progress;

const DEFAULT_BASES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

#[derive(Default)]
pub struct DiffOptions {
    pub base: Option<String>,
    pub side: Side,
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
    /// Metric 1: what the reviewer must newly absorb.
    pub review_bytes: u64,
    /// Metric 2: complexity added (+) or removed (−).
    pub delta_bytes: i64,
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

#[derive(Serialize)]
pub struct DiffReport {
    pub version: VersionInfo,
    pub base: String,
    pub merge_base: String,
    pub files: Vec<DiffFile>,
    pub skipped: Vec<Skipped>,
    pub totals: Totals,
}

#[derive(Default)]
pub struct AbsOptions {
    /// Exclude test files from the universe entirely.
    pub ignore_tests: bool,
    pub side: Side,
}

/// One file's contribution to C(tree): its chain-rule score in sorted-path
/// order; contributions sum to C(tree).
#[derive(Serialize)]
pub struct AbsFile {
    pub path: String,
    pub bytes: u64,
    pub lines: u64,
}

#[derive(Serialize)]
pub struct AbsReport {
    pub version: VersionInfo,
    pub snapshot: &'static str,
    pub file_count: usize,
    /// The kept files' sizes, summed.
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    pub files: Vec<AbsFile>,
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

pub fn diff(git: &Git, opts: &DiffOptions, progress: Progress) -> Result<DiffReport> {
    let base = resolve_base(git, opts.base.as_deref())?;
    let merge_base = git.merge_base(&base, "HEAD")?;
    let changes = git.changes(&merge_base, opts.side)?;

    // Fetch the whole old tree plus the new side of every change.
    let tree_paths = git.ls_tree(&merge_base)?;
    let tree_refs: Vec<&str> = tree_paths.iter().map(String::as_str).collect();
    let old_tree: HashMap<&str, Vec<u8>> = tree_refs
        .iter()
        .copied()
        .zip(git.tree_contents(&merge_base, &tree_refs)?)
        .filter_map(|(p, b)| Some((p, b?)))
        .collect();

    let new_side_paths: Vec<&str> = changes
        .iter()
        .filter(|c| c.status != Status::Deleted)
        .map(|c| c.path.as_str())
        .collect();
    let new_contents: HashMap<&str, Vec<u8>> = new_side_paths
        .iter()
        .copied()
        .zip(git.contents(opts.side, &new_side_paths)?)
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
    let tree: Vec<&[u8]> = kept_tree.iter().map(|p| old_tree[*p].as_slice()).collect();
    let remainder: Vec<&[u8]> = kept_tree
        .iter()
        .copied()
        .filter(|p| !touched.contains(p))
        .map(|p| old_tree[p].as_slice())
        .collect();

    // The three passes (plan §3): metric 1 against the full old tree,
    // metric 2 as new-vs-old against the neutral remainder.
    let new_items: Vec<&[u8]> = items.iter().filter_map(|i| i.new.as_deref()).collect();
    let old_items: Vec<&[u8]> = items.iter().filter_map(|i| i.old.as_deref()).collect();
    let passes = [
        scorer.attribution(&tree, &new_items),
        scorer.attribution(&remainder, &new_items),
        scorer.attribution(&remainder, &old_items),
    ];
    let phase = progress.phase("diff", passes.iter().map(|pass| pass.cost()).sum());
    let [review, delta_new, delta_old] = run_all(passes.each_ref(), &phase);

    let mut files = Vec::with_capacity(items.len());
    let (mut new_i, mut old_i) = (0, 0);
    for item in &items {
        let (review_bytes, new_delta) = if item.new.is_some() {
            let scored = (review[new_i], delta_new[new_i] as i64);
            new_i += 1;
            scored
        } else {
            (0, 0)
        };
        let old_delta = if item.old.is_some() {
            let d = delta_old[old_i] as i64;
            old_i += 1;
            d
        } else {
            0
        };
        let new_lines = item.new.as_deref().map_or(0, lines);
        files.push(DiffFile {
            path: item.path.clone(),
            status: item.status.clone(),
            review_bytes,
            delta_bytes: new_delta - old_delta,
            new_lines,
            bytes_per_line: (item.status == Status::Added && new_lines > 0)
                .then(|| review_bytes as f64 / new_lines as f64),
            density_outlier: false,
        });
    }
    flag_density_outliers(&mut files);
    files.sort_by_key(|f| Reverse(f.review_bytes));

    let churn = git.line_counts(&merge_base, opts.side)?;
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
            review_bytes: review.iter().sum(),
            delta_bytes: delta_new.iter().sum::<u64>() as i64
                - delta_old.iter().sum::<u64>() as i64,
            added_lines,
            deleted_lines,
        },
    })
}

pub fn abs(git: &Git, opts: &AbsOptions, progress: Progress) -> Result<AbsReport> {
    let paths = git.list(opts.side)?;
    let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
    let contents: Vec<(String, Vec<u8>)> = paths
        .iter()
        .cloned()
        .zip(git.contents(opts.side, &path_refs)?)
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
    let tree = scorer.attribution(&[], &kept_contents);
    let scores = tree.run(&progress.phase("C(tree)", tree.cost()));
    let mut files: Vec<AbsFile> = kept
        .iter()
        .zip(&scores)
        .map(|((path, content), &bytes)| AbsFile {
            path: (*path).clone(),
            bytes,
            lines: lines(content),
        })
        .collect();
    files.sort_by_key(|f| Reverse(f.bytes));

    Ok(AbsReport {
        version: VersionInfo::for_scorer(&scorer),
        snapshot: opts.side.label(),
        file_count: kept.len(),
        raw_bytes: kept_contents.iter().map(|c| c.len() as u64).sum(),
        compressed_bytes: scores.iter().sum(),
        files,
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
