//! Orchestrates a scoring run: resolve refs, fetch blobs, filter, build
//! the references, then score every pass the invocation needs in one
//! batch.
//!
//! Fetching and scoring are separate steps so a view that needs both —
//! the overview — hands all its passes to a single batch instead of
//! waiting for one view before starting the next.

use std::cmp::Reverse;
use std::collections::{HashMap, HashSet};

use anyhow::{Result, bail};
use cx_core::{Attribution, Scorer};
use serde::Serialize;

use crate::filter::Filter;
use crate::git::{Change, Git, Side, Status};
use crate::progress::Progress;
use crate::scope::Scope;
use crate::strip;

const DEFAULT_BASES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

#[derive(Default)]
pub struct DiffOptions {
    pub base: Option<String>,
    pub side: Side,
    /// Score test files too. Otherwise they leave the universe entirely:
    /// in no reference and no scoring pass, and reported as skipped.
    pub include_tests: bool,
    /// Score comments too. Otherwise every blob is reduced to code before
    /// it enters any reference or scoring pass.
    pub comments: bool,
    /// Score prose files (Markdown, reStructuredText, …) too. Otherwise
    /// they are skipped, like tests.
    pub prose: bool,
    /// Restrict the run to the paths these globs select — see [`Scope`].
    /// Empty is the whole repository.
    pub globs: Vec<String>,
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

/// What a blob becomes on its way into scoring: its code, or the reason
/// the filter dropped it. The filter sees raw bytes (binary detection
/// needs them); everything after sees code only.
type Prepared = Result<Vec<u8>, &'static str>;

fn prepare(filter: &Filter, comments: bool, path: &str, raw: Vec<u8>) -> Prepared {
    match filter.exclusion(path, &raw) {
        Some(reason) => Err(reason),
        None if comments => Ok(raw),
        None => Ok(strip::code_only(path, raw)),
    }
}

/// Every path with a blob, prepared, in the order `paths` was given.
/// Paths whose blob is missing (a submodule, a file gone from disk) are
/// simply absent.
///
/// Preparing a blob is a pure function of that blob, so the load is one
/// parallel map: the cores split the paths between them and the results
/// are stitched back into the caller's order.
fn load<'a>(
    filter: &Filter,
    comments: bool,
    paths: &[&'a str],
    blobs: Vec<Option<Vec<u8>>>,
) -> Vec<(&'a str, Prepared)> {
    let mut rest: Vec<(&'a str, Vec<u8>)> = paths
        .iter()
        .copied()
        .zip(blobs)
        .filter_map(|(path, blob)| Some((path, blob?)))
        .collect();
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per_core = rest.len().div_ceil(cores).max(1);
    let mut lanes: Vec<Vec<(&'a str, Vec<u8>)>> = Vec::new();
    while !rest.is_empty() {
        let tail = rest.split_off(per_core.min(rest.len()));
        lanes.push(std::mem::replace(&mut rest, tail));
    }
    std::thread::scope(|scope| {
        lanes
            .into_iter()
            .map(|lane| {
                scope.spawn(move || {
                    lane.into_iter()
                        .map(|(path, raw)| (path, prepare(filter, comments, path, raw)))
                        .collect::<Vec<_>>()
                })
            })
            .collect::<Vec<_>>()
            .into_iter()
            .flat_map(|lane| lane.join().expect("prepare thread"))
            .collect()
    })
}

/// The code of a prepared blob, or None if it was dropped (or absent).
fn code(prepared: Option<&Prepared>) -> Option<&[u8]> {
    prepared?.as_deref().ok()
}

/// One zstd stream to run: the bytes the compressor is conditioned on,
/// then the items it attributes.
pub struct Pass<'a> {
    reference: Vec<&'a [u8]>,
    items: Vec<&'a [u8]>,
}

/// Score every pass of one invocation. The passes are independent
/// streams, so they run concurrently, and the bar spans all the bytes
/// the invocation will compress rather than one view's share of them.
fn score<const N: usize>(
    scorer: &Scorer,
    label: &'static str,
    passes: [Pass; N],
    progress: Progress,
) -> [Vec<u64>; N] {
    let streams = passes
        .each_ref()
        .map(|pass| scorer.attribution(&pass.reference, &pass.items));
    let bar = &progress.bar(label, streams.iter().map(Attribution::bytes).sum());
    std::thread::scope(|scope| {
        streams
            .each_ref()
            .map(|stream| scope.spawn(move || stream.run(bar)))
            .map(|stream| stream.join().expect("stream thread"))
    })
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
    /// Score test files too; otherwise they leave the universe entirely.
    pub include_tests: bool,
    /// Score comments too; otherwise every blob is reduced to code first.
    pub comments: bool,
    /// Score prose files too; otherwise they are skipped.
    pub prose: bool,
    pub side: Side,
    /// Restrict the run to the paths these globs select — see [`Scope`].
    /// Empty is the whole repository.
    pub globs: Vec<String>,
}

/// One file's contribution to C(tree): its sequential chain-rule score in
/// sorted-path order.
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
    pub raw_bytes: u64,
    pub compressed_bytes: u64,
    pub files: Vec<AbsFile>,
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

/// The path the base tree holds a change's old side under, if it has
/// one: its own, or the name it was renamed from.
fn old_path(change: &Change) -> Option<&str> {
    match &change.status {
        Status::Added => None,
        Status::Modified | Status::Deleted => Some(&change.path),
        Status::Renamed { from } => Some(from),
    }
}

/// The code an abs run scores, fetched and prepared. The blobs are owned
/// apart from the pass that reads them, which is what lets the overview
/// score this view alongside the diff's.
pub struct AbsSources {
    snapshot: &'static str,
    /// Each kept blob, filtered and reduced to code, in the order
    /// `git.list` produced — already sorted, which the chain rule wants.
    kept: Vec<(String, Vec<u8>)>,
}

impl AbsSources {
    pub fn fetch(git: &Git, opts: &AbsOptions) -> Result<Self> {
        // Scoped before the blobs are fetched: out of scope is out of the
        // repository, as far as this run is concerned.
        let scope = Scope::new(git.root(), &opts.globs)?;
        let mut paths = git.list(opts.side)?;
        paths.retain(|p| scope.allows(p));
        let path_refs: Vec<&str> = paths.iter().map(String::as_str).collect();
        let blobs = git.contents(opts.side, &path_refs)?;
        let attr_paths: Vec<String> = path_refs
            .iter()
            .zip(&blobs)
            .filter_map(|(p, b)| b.as_ref().map(|_| p.to_string()))
            .collect();
        let filter = Filter::new(
            git.root(),
            git.linguist_attrs(&attr_paths)?,
            opts.include_tests,
            opts.prose,
        )?;
        Ok(AbsSources {
            snapshot: opts.side.label(),
            kept: load(&filter, opts.comments, &path_refs, blobs)
                .into_iter()
                .filter_map(|(path, prepared)| Some((path.to_owned(), prepared.ok()?)))
                .collect(),
        })
    }

    /// C(tree): one chain-rule pass over the whole snapshot, with no
    /// reference — each file is conditioned on the ones before it.
    pub fn passes(&self) -> [Pass<'_>; 1] {
        [Pass {
            reference: Vec::new(),
            items: self.kept.iter().map(|(_, code)| code.as_slice()).collect(),
        }]
    }

    pub fn report(&self, scorer: &Scorer, [tree]: [Vec<u64>; 1]) -> AbsReport {
        let mut files: Vec<AbsFile> = self
            .kept
            .iter()
            .zip(&tree)
            .map(|((path, content), &bytes)| AbsFile {
                path: path.clone(),
                bytes,
                lines: lines(content),
            })
            .collect();
        files.sort_by_key(|f| Reverse(f.bytes));
        AbsReport {
            version: VersionInfo::for_scorer(scorer),
            snapshot: self.snapshot,
            file_count: self.kept.len(),
            raw_bytes: self.kept.iter().map(|(_, code)| code.len() as u64).sum(),
            compressed_bytes: tree.iter().sum(),
            files,
        }
    }
}

/// The code a diff run scores, fetched and prepared: both sides' blobs,
/// the changes worth scoring, and the churn git counted.
pub struct DiffSources {
    base: String,
    merge_base: String,
    /// In-scope paths of the base tree, in the sorted order git produced.
    tree_paths: Vec<String>,
    old_tree: HashMap<String, Prepared>,
    new_contents: HashMap<String, Prepared>,
    /// Every path a change occupies on either side — what the delta
    /// passes' neutral reference leaves out.
    touched: HashSet<String>,
    /// The changes with at least one scorable side, sorted by path.
    changes: Vec<Change>,
    skipped: Vec<Skipped>,
    churn: HashMap<String, (u64, u64)>,
}

impl DiffSources {
    pub fn fetch(git: &Git, opts: &DiffOptions) -> Result<Self> {
        let base = resolve_base(git, opts.base.as_deref())?;
        let merge_base = git.merge_base(&base, "HEAD")?;
        // Scoping happens on the raw listings, before a blob is fetched: an
        // out-of-scope path is not in the reference, not scored, and not
        // skipped — it is simply not part of this run's repository.
        let scope = Scope::new(git.root(), &opts.globs)?;
        let mut changes = git.changes(&merge_base, opts.side)?;
        changes.retain(|c| scope.allows(&c.path));
        let mut tree_paths = git.ls_tree(&merge_base)?;
        tree_paths.retain(|p| scope.allows(p));
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
        let filter = Filter::new(
            git.root(),
            git.linguist_attrs(&attr_paths)?,
            opts.include_tests,
            opts.prose,
        )?;

        // The whole old tree plus the new side of every change, each blob
        // filtered and reduced to code once.
        let owned = |loaded: Vec<(&str, Prepared)>| -> HashMap<String, Prepared> {
            loaded
                .into_iter()
                .map(|(path, prepared)| (path.to_owned(), prepared))
                .collect()
        };
        let old_tree = owned(load(
            &filter,
            opts.comments,
            &tree_refs,
            git.tree_contents(&merge_base, &tree_refs)?,
        ));
        let new_contents = owned(load(
            &filter,
            opts.comments,
            &new_side_paths,
            git.contents(opts.side, &new_side_paths)?,
        ));

        // Partition changes into scorable ones and skipped files. A change
        // is skipped when any side it has fails the filter (e.g. a file that
        // flipped binary→text is skipped whole rather than half-scored).
        let mut scorable: Vec<Change> = Vec::new();
        let mut skipped: Vec<Skipped> = Vec::new();
        let mut touched: HashSet<String> = HashSet::new();
        for change in &changes {
            touched.insert(change.path.clone());
            if let Status::Renamed { from } = &change.status {
                touched.insert(from.clone());
            }
            let old = old_path(change).and_then(|p| old_tree.get(p));
            let new = (change.status != Status::Deleted)
                .then(|| new_contents.get(&change.path))
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
            if code(old).is_none() && code(new).is_none() {
                continue;
            }
            scorable.push(change.clone());
        }
        scorable.sort_by(|a, b| a.path.cmp(&b.path));

        Ok(DiffSources {
            churn: git.line_counts(&merge_base, opts.side)?,
            base,
            merge_base,
            tree_paths,
            old_tree,
            new_contents,
            touched,
            changes: scorable,
            skipped,
        })
    }

    /// A change's old side: the file as the base tree had it. A side a
    /// change does not have is the empty file, which scores 0, so every
    /// pass is indexed like `changes`.
    fn old_side(&self, change: &Change) -> &[u8] {
        old_path(change)
            .and_then(|path| code(self.old_tree.get(path)))
            .unwrap_or_default()
    }

    /// A change's new side, by the same rule.
    fn new_side(&self, change: &Change) -> &[u8] {
        (change.status != Status::Deleted)
            .then(|| code(self.new_contents.get(&change.path)))
            .flatten()
            .unwrap_or_default()
    }

    /// The universe is kept files only: a file the filter excludes exists
    /// in no reference and no scoring pass.
    fn kept_tree(&self) -> impl Iterator<Item = (&str, &[u8])> {
        self.tree_paths
            .iter()
            .filter_map(|path| Some((path.as_str(), code(self.old_tree.get(path))?)))
    }

    /// Metric 1 is new against the full old tree; metric 2 is new minus
    /// old, each against the neutral remainder.
    pub fn passes(&self) -> [Pass<'_>; 3] {
        let tree: Vec<&[u8]> = self.kept_tree().map(|(_, code)| code).collect();
        let remainder: Vec<&[u8]> = self
            .kept_tree()
            .filter(|(path, _)| !self.touched.contains(*path))
            .map(|(_, code)| code)
            .collect();
        let new: Vec<&[u8]> = self.changes.iter().map(|c| self.new_side(c)).collect();
        let old: Vec<&[u8]> = self.changes.iter().map(|c| self.old_side(c)).collect();
        [
            Pass {
                reference: tree,
                items: new.clone(),
            },
            Pass {
                reference: remainder.clone(),
                items: new,
            },
            Pass {
                reference: remainder,
                items: old,
            },
        ]
    }

    pub fn report(
        &self,
        scorer: &Scorer,
        [review, delta_new, delta_old]: [Vec<u64>; 3],
    ) -> DiffReport {
        let mut files: Vec<DiffFile> = self
            .changes
            .iter()
            .enumerate()
            .map(|(i, change)| {
                let new_lines = lines(self.new_side(change));
                DiffFile {
                    path: change.path.clone(),
                    status: change.status.clone(),
                    review_bytes: review[i],
                    delta_bytes: delta_new[i] as i64 - delta_old[i] as i64,
                    new_lines,
                    bytes_per_line: (change.status == Status::Added && new_lines > 0)
                        .then(|| review[i] as f64 / new_lines as f64),
                    density_outlier: false,
                }
            })
            .collect();
        flag_density_outliers(&mut files);
        files.sort_by_key(|f| Reverse(f.review_bytes));

        let (added_lines, deleted_lines) = self
            .changes
            .iter()
            .filter_map(|change| self.churn.get(&change.path))
            .fold((0, 0), |(a, d), (added, deleted)| (a + added, d + deleted));

        DiffReport {
            version: VersionInfo::for_scorer(scorer),
            base: self.base.clone(),
            merge_base: self.merge_base.clone(),
            totals: Totals {
                review_bytes: files.iter().map(|f| f.review_bytes).sum(),
                delta_bytes: files.iter().map(|f| f.delta_bytes).sum(),
                added_lines,
                deleted_lines,
            },
            files,
            skipped: self
                .skipped
                .iter()
                .map(|skip| Skipped {
                    path: skip.path.clone(),
                    reason: skip.reason.clone(),
                })
                .collect(),
        }
    }
}

pub fn diff(git: &Git, opts: &DiffOptions, progress: Progress) -> Result<DiffReport> {
    let sources = DiffSources::fetch(git, opts)?;
    let scorer = Scorer::default();
    let scores = score(&scorer, "diff", sources.passes(), progress);
    Ok(sources.report(&scorer, scores))
}

pub fn abs(git: &Git, opts: &AbsOptions, progress: Progress) -> Result<AbsReport> {
    let sources = AbsSources::fetch(git, opts)?;
    let scorer = Scorer::default();
    let scores = score(&scorer, "C(tree)", sources.passes(), progress);
    Ok(sources.report(&scorer, scores))
}

/// Both views at once. Their four passes are independent streams, so the
/// whole invocation is one scoring batch: C(tree) no longer waits for the
/// diff to finish, and the bar covers both.
pub fn overview(
    git: &Git,
    abs_opts: &AbsOptions,
    diff_opts: &DiffOptions,
    progress: Progress,
) -> Result<(AbsReport, DiffReport)> {
    // Two independent trips to git and back; neither view waits on the
    // other to have its blobs.
    let (tree_sources, diff_sources) = std::thread::scope(|scope| {
        let tree = scope.spawn(|| AbsSources::fetch(git, abs_opts));
        let diff = DiffSources::fetch(git, diff_opts);
        (tree.join().expect("fetch thread"), diff)
    });
    let (tree_sources, diff_sources) = (tree_sources?, diff_sources?);
    let scorer = Scorer::default();
    let [tree] = tree_sources.passes();
    let [review, delta_new, delta_old] = diff_sources.passes();
    let [tree, review, delta_new, delta_old] = score(
        &scorer,
        "scoring",
        [tree, review, delta_new, delta_old],
        progress,
    );
    Ok((
        tree_sources.report(&scorer, [tree]),
        diff_sources.report(&scorer, [review, delta_new, delta_old]),
    ))
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
