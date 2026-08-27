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
use crate::git::{Git, Side, Status};
use crate::progress::Progress;
use crate::scope::Scope;
use crate::strip;

const DEFAULT_BASES: [&str; 4] = ["main", "master", "origin/main", "origin/master"];

/// What a scoring run selects; every view shares the same choices. The
/// overview scores two views of one invocation, and they must agree on
/// which files exist or the merged report describes two repositories.
#[derive(Default)]
pub struct Options {
    pub side: Side,
    /// Score test files too. Otherwise they leave the universe entirely:
    /// in no reference and no scoring pass, and reported as skipped.
    pub include_tests: bool,
    /// Stripped-by-default byte classes — comments, string literal
    /// contents — to score anyway. Otherwise every blob is reduced to
    /// code before it enters any reference or scoring pass.
    pub keep: strip::Keep,
    /// Score prose files (Markdown, reStructuredText, …) too. Otherwise
    /// they are skipped, like tests.
    pub prose: bool,
    /// Score data files (JSON, CSV, …) too. Otherwise they are skipped,
    /// like prose.
    pub data: bool,
    /// Restrict the run to the paths these globs select — see [`Scope`].
    /// Empty is the whole repository.
    pub globs: Vec<String>,
}

impl Options {
    /// The file filter these selections configure.
    fn filter(&self, git: &Git, attr_paths: &[String]) -> Result<Filter> {
        Filter::new(
            git.root(),
            git.linguist_attrs(attr_paths)?,
            self.include_tests,
            self.prose,
            self.data,
        )
    }
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
    /// Diff churn: lines this file adds and removes against the merge base.
    /// The signed net (added − deleted) is the diff view's LINES column.
    pub added_lines: u64,
    pub deleted_lines: u64,
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

fn prepare(filter: &Filter, keep: strip::Keep, path: &str, raw: Vec<u8>) -> Prepared {
    match filter.exclusion(path, &raw) {
        Some(reason) => Err(reason),
        None => Ok(strip::code_only(path, raw, keep)),
    }
}

/// Every path with a blob, prepared, in the order `paths` was given.
/// Paths whose blob is missing (a submodule, a file gone from disk) are
/// simply absent. Preparing a blob is a pure function of that blob, so
/// the paths are cut into one lane per core and prepared at once — into
/// contiguous lanes, because the chain rule scores a file against the
/// ones before it and a permutation would silently change every score.
fn load<'a>(
    filter: &Filter,
    keep: strip::Keep,
    paths: &[&'a str],
    mut blobs: Vec<Option<Vec<u8>>>,
) -> Vec<(&'a str, Prepared)> {
    let cores = std::thread::available_parallelism().map_or(1, |n| n.get());
    let per_lane = blobs.len().div_ceil(cores).max(1);
    std::thread::scope(|scope| {
        paths
            .chunks(per_lane)
            .zip(blobs.chunks_mut(per_lane))
            .map(|(paths, blobs)| {
                scope.spawn(move || {
                    paths
                        .iter()
                        .zip(blobs)
                        .filter_map(|(path, blob)| {
                            Some((*path, prepare(filter, keep, path, blob.take()?)))
                        })
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

/// One stream: `items` attributed against `reference` — or against
/// nothing, when every item is empty. Such a pass scores 0 for every one
/// of them however large its reference is, so conditioning it on nothing
/// leaves the same scores and the compressor never reads the tree;
/// `cx_core`'s `an_all_empty_pass_is_zero_whatever_its_reference` pins
/// that the two forms agree.
fn pass<'a>(scorer: &'a Scorer, reference: &[&'a [u8]], items: &[&'a [u8]]) -> Attribution<'a> {
    match items.iter().all(|item| item.is_empty()) {
        true => scorer.attribution(&[], items),
        false => scorer.attribution(reference, items),
    }
}

/// Score every pass of one invocation. The passes are independent
/// streams, so they run concurrently, and the bar spans all the bytes
/// the invocation will compress rather than one view's share of them.
fn score<const N: usize>(
    label: &'static str,
    passes: [Attribution; N],
    progress: Progress,
) -> [Vec<u64>; N] {
    let bar = &progress.bar(label, passes.iter().map(|pass| pass.bytes()).sum());
    std::thread::scope(|scope| {
        passes
            .each_ref()
            .map(|pass| scope.spawn(move || pass.run(bar)))
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

/// One changed file with whichever sides exist and passed the filter. A
/// side the change does not have is the empty file, which scores 0, so
/// every pass is indexed like `items`.
struct Item {
    path: String,
    status: Status,
    old: Vec<u8>,
    new: Vec<u8>,
}

/// The code an abs run scores, fetched and prepared. The blobs are owned
/// apart from the pass that reads them, which is what lets the overview
/// score this view alongside the diff's.
struct AbsSources {
    snapshot: &'static str,
    /// Each kept blob, filtered and reduced to code, in the order
    /// `git.list` produced — already sorted, which the chain rule wants.
    kept: Vec<(String, Vec<u8>)>,
}

impl AbsSources {
    fn fetch(git: &Git, opts: &Options) -> Result<Self> {
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
        let filter = opts.filter(git, &attr_paths)?;
        Ok(AbsSources {
            snapshot: opts.side.label(),
            kept: load(&filter, opts.keep, &path_refs, blobs)
                .into_iter()
                .filter_map(|(path, prepared)| Some((path.to_owned(), prepared.ok()?)))
                .collect(),
        })
    }

    /// C(tree): one chain-rule pass over the whole snapshot, with no
    /// reference — each file is conditioned on the ones before it.
    fn passes<'a>(&'a self, scorer: &'a Scorer) -> [Attribution<'a>; 1] {
        let kept: Vec<&[u8]> = self.kept.iter().map(|(_, code)| code.as_slice()).collect();
        [scorer.attribution(&[], &kept)]
    }

    fn report(self, scorer: &Scorer, [tree]: [Vec<u64>; 1]) -> AbsReport {
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

/// The code a diff run scores, fetched and prepared: the base tree, the
/// items worth scoring, and the churn git counted.
struct DiffSources {
    base: String,
    merge_base: String,
    /// The kept base tree in the sorted order git produced, each file's
    /// code paired with whether a change touches it — the flag is what
    /// the delta passes' neutral reference leaves out.
    tree: Vec<(bool, Vec<u8>)>,
    /// The changes with at least one scorable side, sorted by path.
    items: Vec<Item>,
    skipped: Vec<Skipped>,
    /// Lines added and deleted per repository path, as git counted them.
    churn: HashMap<String, (u64, u64)>,
}

impl DiffSources {
    fn fetch(git: &Git, base: Option<&str>, opts: &Options) -> Result<Self> {
        let base = resolve_base(git, base)?;
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
        let filter = opts.filter(git, &attr_paths)?;

        // The whole old tree plus the new side of every change, each blob
        // filtered and reduced to code once.
        let mut old_tree: HashMap<&str, Prepared> = load(
            &filter,
            opts.keep,
            &tree_refs,
            git.tree_contents(&merge_base, &tree_refs)?,
        )
        .into_iter()
        .collect();
        let new_contents: HashMap<&str, Prepared> = load(
            &filter,
            opts.keep,
            &new_side_paths,
            git.contents(opts.side, &new_side_paths)?,
        )
        .into_iter()
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
                old: old.unwrap_or_default().to_vec(),
                new: new.unwrap_or_default().to_vec(),
            });
        }
        items.sort_by(|a, b| a.path.cmp(&b.path));

        // The universe is kept files only: a file the filter excludes exists
        // in no reference and no scoring pass.
        let tree: Vec<(bool, Vec<u8>)> = tree_refs
            .iter()
            .filter_map(|p| Some((touched.contains(p), old_tree.remove(p)?.ok()?)))
            .collect();

        Ok(DiffSources {
            churn: git.line_counts(&merge_base, opts.side)?,
            base,
            merge_base,
            tree,
            items,
            skipped,
        })
    }

    /// Metric 1 is new against the full old tree; metric 2 is new minus
    /// old, each against the neutral remainder.
    fn passes<'a>(&'a self, scorer: &'a Scorer) -> [Attribution<'a>; 3] {
        let tree: Vec<&[u8]> = self.tree.iter().map(|(_, code)| code.as_slice()).collect();
        let remainder: Vec<&[u8]> = self
            .tree
            .iter()
            .filter(|(touched, _)| !touched)
            .map(|(_, code)| code.as_slice())
            .collect();
        let new: Vec<&[u8]> = self.items.iter().map(|i| i.new.as_slice()).collect();
        let old: Vec<&[u8]> = self.items.iter().map(|i| i.old.as_slice()).collect();
        [
            pass(scorer, &tree, &new),
            pass(scorer, &remainder, &new),
            pass(scorer, &remainder, &old),
        ]
    }

    fn report(self, scorer: &Scorer, [review, delta_new, delta_old]: [Vec<u64>; 3]) -> DiffReport {
        let mut files = Vec::with_capacity(self.items.len());
        for (i, item) in self.items.iter().enumerate() {
            let new_lines = lines(&item.new);
            let (added_lines, deleted_lines) =
                self.churn.get(&item.path).copied().unwrap_or_default();
            files.push(DiffFile {
                path: item.path.clone(),
                status: item.status.clone(),
                review_bytes: review[i],
                delta_bytes: delta_new[i] as i64 - delta_old[i] as i64,
                added_lines,
                deleted_lines,
                new_lines,
                bytes_per_line: (item.status == Status::Added && new_lines > 0)
                    .then(|| review[i] as f64 / new_lines as f64),
                density_outlier: false,
            });
        }
        flag_density_outliers(&mut files);
        files.sort_by_key(|f| Reverse(f.review_bytes));

        DiffReport {
            version: VersionInfo::for_scorer(scorer),
            base: self.base,
            merge_base: self.merge_base,
            totals: Totals {
                review_bytes: files.iter().map(|f| f.review_bytes).sum(),
                delta_bytes: files.iter().map(|f| f.delta_bytes).sum(),
                added_lines: files.iter().map(|f| f.added_lines).sum(),
                deleted_lines: files.iter().map(|f| f.deleted_lines).sum(),
            },
            files,
            skipped: self.skipped,
        }
    }
}

pub fn diff(
    git: &Git,
    base: Option<&str>,
    opts: &Options,
    progress: Progress,
) -> Result<DiffReport> {
    let sources = DiffSources::fetch(git, base, opts)?;
    let scorer = Scorer::default();
    let scores = score("diff", sources.passes(&scorer), progress);
    Ok(sources.report(&scorer, scores))
}

pub fn abs(git: &Git, opts: &Options, progress: Progress) -> Result<AbsReport> {
    let sources = AbsSources::fetch(git, opts)?;
    let scorer = Scorer::default();
    let scores = score("C(tree)", sources.passes(&scorer), progress);
    Ok(sources.report(&scorer, scores))
}

/// Both views at once. Their four passes are independent streams, so the
/// whole invocation is one scoring batch: C(tree) no longer waits for the
/// diff to finish, and the bar covers both.
///
/// The time is bought with memory. Running the views together holds both
/// their corpora and runs four compressors at once, so this peaks at
/// about the sum of what the two views peak at separately rather than
/// the larger of the two.
pub fn overview(
    git: &Git,
    base: Option<&str>,
    opts: &Options,
    progress: Progress,
) -> Result<(AbsReport, DiffReport)> {
    // Two independent trips to git and back; neither view waits on the
    // other to have its blobs.
    let (tree_sources, diff_sources) = std::thread::scope(|scope| {
        let tree = scope.spawn(|| AbsSources::fetch(git, opts));
        let diff = DiffSources::fetch(git, base, opts);
        anyhow::Ok((tree.join().expect("fetch thread")?, diff?))
    })?;
    let scorer = Scorer::default();
    let [tree] = tree_sources.passes(&scorer);
    let [review, delta_new, delta_old] = diff_sources.passes(&scorer);
    let [tree, review, delta_new, delta_old] =
        score("scoring", [tree, review, delta_new, delta_old], progress);
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

/// The filter stack's density backstop: flag (never drop) files whose
/// density is far off this run's median — probable generated/vendored
/// content that no pattern anticipated.
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
