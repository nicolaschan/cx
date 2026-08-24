//! Human-readable rendering via comfy-table: the layout and the tables'
//! styling come from the library. The JSON form is serde on the report
//! structs — that serialization is the contract tooling consumes.

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets};
use crossterm::style::{ResetColor, SetForegroundColor};

use crate::breakdown::{self, Entry, Node};
use crate::git::Status;
use crate::pipeline::{AbsReport, DiffFile, DiffReport, VersionInfo};

/// What a view includes, and whether it may say it in color — an input,
/// not something the renderer reads off the process it runs in, so the
/// same report renders the same bytes wherever it runs.
#[derive(Clone, Copy)]
pub struct Options {
    /// Only the N biggest files/directories in the breakdown.
    pub top: usize,
    pub files: bool,
    /// The footer's detail lines.
    pub verbose: bool,
    /// Tables and footer alike: a run colors everything or nothing.
    pub color: bool,
}

impl Options {
    /// Colors are the `comfy_table::Color` the table cells use (the
    /// crate re-exports crossterm's type), so a value means the same
    /// thing above and below the table.
    fn paint(self, text: impl std::fmt::Display, color: Option<Color>) -> String {
        match color.filter(|_| self.color) {
            Some(c) => format!("{}{text}{}", SetForegroundColor(c), ResetColor),
            None => text.to_string(),
        }
    }

    /// Incidental metadata: present, never competing with the numbers.
    fn dim(self, text: impl std::fmt::Display) -> String {
        self.paint(text, Some(Color::DarkGrey))
    }

    /// A `label value` pair: the label dim, the value carrying the color.
    fn stat(self, label: &str, value: String, color: Option<Color>) -> String {
        format!("{} {}", self.dim(label), self.paint(value, color))
    }
}

/// An [`Entry`] for one path, with the diff columns (ΔC + marker) filled
/// from its change when it has one. The single construction point for
/// every view.
fn entry<'a>(path: &'a str, bytes: f64, lines: u64, change: Option<&DiffFile>) -> Entry<'a> {
    Entry {
        path,
        bytes,
        lines,
        delta: change.map(|c| c.delta_bytes),
        marker: change.and_then(status_marker),
    }
}

/// Diff-status indicator: "+" added, "−" deleted, "→ <from>" renamed,
/// "⚠" appended for density outliers.
fn status_marker(file: &DiffFile) -> Option<String> {
    let base = match &file.status {
        Status::Added => Some("+".to_owned()),
        Status::Deleted => Some("−".to_owned()),
        Status::Renamed { from } => Some(format!("→ {from}")),
        Status::Modified => None,
    };
    if file.density_outlier {
        Some(format!("{} ⚠", base.unwrap_or_default()).trim().to_owned())
    } else {
        base
    }
}

/// Diff-status colors follow the universal diff convention.
fn marker_color(marker: &str) -> Option<Color> {
    match marker.chars().next() {
        _ if marker.contains('⚠') => Some(Color::Yellow),
        Some('+') => Some(Color::Green),
        Some('−') => Some(Color::Red),
        Some('→') => Some(Color::Cyan),
        _ => None,
    }
}

/// Magnitude coloring shared by both metrics: tiny scores fade, big ones
/// warn. Negative deltas (removed complexity) are the one good color.
fn score_color(bytes: f64) -> Option<Color> {
    if bytes.abs() < 64.0 {
        Some(Color::DarkGrey)
    } else if bytes <= -64.0 {
        Some(Color::Green)
    } else if bytes >= 4096.0 {
        Some(Color::Red)
    } else if bytes >= 1024.0 {
        Some(Color::Yellow)
    } else {
        None
    }
}

fn colored(cell: Cell, color: Option<Color>) -> Cell {
    match color {
        Some(c) => cell.fg(c),
        None => cell,
    }
}

fn num_cell(text: String, color: Option<Color>) -> Cell {
    colored(Cell::new(text).set_alignment(CellAlignment::Right), color)
}

/// One rendered line of the dust-style breakdown. Elision-summary rows
/// (`dim`) render entirely gray.
struct Row {
    bytes: f64,
    delta: Option<f64>,
    marker: Option<String>,
    lines: Option<u64>,
    label: String,
    is_dir: bool,
    dim: bool,
}

impl Row {
    fn push_onto(self, table: &mut Table, total: f64, show_delta: bool) {
        let dim = self.dim.then_some(Color::DarkGrey);
        let share = 100.0 * self.bytes / total;
        let filled = ((share / 10.0).round() as usize).min(10);
        let mut cells = vec![num_cell(
            fmt_bytes(self.bytes),
            dim.or_else(|| score_color(self.bytes)),
        )];
        if show_delta {
            cells.push(self.delta.map_or_else(
                || Cell::new(""),
                |d| num_cell(fmt_signed(d), dim.or_else(|| score_color(d))),
            ));
            let marker = self.marker.unwrap_or_default();
            let color = dim.or_else(|| marker_color(&marker));
            cells.push(colored(Cell::new(marker), color));
        }
        let path_style = dim.map(|_| Color::DarkGrey);
        let mut path_cell = colored(Cell::new(self.label), path_style);
        if self.is_dir {
            path_cell = path_cell.add_attribute(Attribute::Bold);
        }
        cells.extend([
            num_cell(self.lines.map_or("-".to_owned(), |l| l.to_string()), dim),
            path_cell,
            num_cell(
                format!(
                    "{}{}  {share:>4.1}%",
                    "█".repeat(filled),
                    "░".repeat(10 - filled)
                ),
                dim.or((share >= 25.0).then_some(Color::Yellow)),
            ),
        ]);
        table.add_row(cells);
    }
}

/// Emit a node's children as rows, biggest first within each directory,
/// with an elision summary last where pruning bit.
fn push_children(table: &mut Table, node: &Node, prefix: &str, total: f64, show_delta: bool) {
    let child_count = node.children.len() + usize::from(node.elided.is_some());
    for (i, child) in node.children.iter().enumerate() {
        let is_last = i + 1 == child_count;
        let connector = if is_last { "└─" } else { "├─" };
        let tip = if child.children.is_empty() && child.elided.is_none() {
            "─ "
        } else {
            "┬ "
        };
        Row {
            bytes: child.bytes,
            delta: child.delta,
            marker: child.marker.clone(),
            lines: Some(child.lines),
            label: format!("{prefix}{connector}{tip}{}", child.name),
            is_dir: child.is_dir,
            dim: false,
        }
        .push_onto(table, total, show_delta);
        let child_prefix = format!("{prefix}{}", if is_last { "  " } else { "│ " });
        push_children(table, child, &child_prefix, total, show_delta);
    }
    if let Some(elided) = &node.elided {
        Row {
            bytes: elided.bytes,
            delta: elided.delta,
            marker: None,
            lines: None,
            label: format!("{prefix}└── … +{} more", elided.count),
            is_dir: false,
            dim: true,
        }
        .push_onto(table, total, show_delta);
    }
}

/// A view: `footer`, under the dust-style breakdown of `entries` where
/// there is one. `diff_columns` names the size column ("BYTES" for tree
/// contributions, "REVIEW" for diff cost) and adds the ΔC + status
/// columns; `None` renders the plain tree view.
fn view<'a>(
    entries: impl IntoIterator<Item = Entry<'a>>,
    total: f64,
    opts: Options,
    diff_columns: Option<&'static str>,
    footer: String,
) -> String {
    if !opts.files {
        return footer;
    }
    let root = breakdown::breakdown(entries, opts.top);
    if root.children.is_empty() {
        return footer;
    }
    let columns: &[&str] = match diff_columns {
        Some(bytes_header) => &[bytes_header, "ΔC", "", "LINES", "PATH", "SHARE"],
        None => &["BYTES", "LINES", "PATH", "SHARE"],
    };
    let mut table = Table::new();
    if opts.color {
        table.enforce_styling();
    } else {
        table.force_no_tty();
    }
    table.load_preset(presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(columns.iter().map(|c| Cell::new(c).fg(Color::DarkGrey)));
    push_children(&mut table, &root, "", total, diff_columns.is_some());
    format!("{table}\n\n{footer}")
}

/// Everything under the table: one summary line, plus the details
/// `--verbose` adds. Every view folds in here, so what they share is
/// written once.
fn footer(
    opts: Options,
    version: &VersionInfo,
    abs: Option<&AbsReport>,
    diff: Option<&DiffReport>,
) -> String {
    let mut summary = Vec::new();
    let mut details = Vec::new();
    if let Some(abs) = abs {
        // C(tree) is a whole-repo absolute, not a change: no magnitude
        // color (it would sit permanently red).
        summary.push(opts.stat("C(tree)", fmt_bytes(abs.compressed_bytes as f64), None));
        details.push(opts.dim(format!(
            "C(tree) over {} files ({} raw)",
            abs.file_count,
            fmt_bytes(abs.raw_bytes as f64),
        )));
    }
    if let Some(diff) = diff {
        // The totals carry the same magnitude coloring as the cells they
        // sum, so a red total and a red row mean one thing.
        let review = diff.totals.review_bytes as f64;
        let delta = diff.totals.delta_bytes as f64;
        if diff.files.is_empty() && diff.skipped.is_empty() {
            summary.push(opts.dim(format!("no scorable changes against {}", diff.base)));
        } else {
            summary.push(opts.stat("review", fmt_bytes(review), score_color(review)));
            summary.push(opts.stat("ΔC", fmt_signed(delta), score_color(delta)));
            // The familiar size ΔC is read against, not a verdict of its
            // own, so it stays uncolored.
            let (added, deleted) = (diff.totals.added_lines, diff.totals.deleted_lines);
            summary.push(opts.stat("lines", format!("+{added} −{deleted}"), None));
        }
        if !diff.skipped.is_empty() {
            // The count stays on the summary line: it says the totals
            // beside it do not cover everything that changed.
            summary.push(opts.dim(format!("{} skipped", diff.skipped.len())));
            let list: Vec<String> = diff
                .skipped
                .iter()
                .map(|s| format!("{} ({})", s.path, s.reason))
                .collect();
            details.push(opts.dim(format!("skipped: {}", list.join(", "))));
        }
    }
    let mut lines = vec![summary.join("   ")];
    if opts.verbose {
        details.push(format!(
            "{}   {}",
            scale_gauge(opts, abs, diff),
            // Provenance: the scores mean nothing without it, but it
            // never changes run to run — dim.
            opts.dim(format!(
                "zstd {}, level {}, window≤2^{}",
                version.zstd, version.level, version.max_window_log
            )),
        ));
        lines.append(&mut details);
    }
    lines.iter().map(|line| format!(" {line}\n")).collect()
}

/// The attribution noise gauge, colored by whether per-item numbers can
/// be trusted at all. It reports the worst of every pass the view
/// merged: one bad pass makes every per-item number in it suspect.
fn scale_gauge(opts: Options, abs: Option<&AbsReport>, diff: Option<&DiffReport>) -> String {
    let scales = diff
        .into_iter()
        .flat_map(|d| [d.scales.review, d.scales.delta_new, d.scales.delta_old]);
    let worst = abs
        .map(|a| a.scale)
        .into_iter()
        .chain(scales)
        .fold(1.0f64, |acc, s| {
            if (s - 1.0).abs() > (acc - 1.0).abs() {
                s
            } else {
                acc
            }
        });
    let (verdict, color) = if (0.7..=1.1).contains(&worst) {
        ("ok", Color::Green)
    } else {
        (
            "noisy — trust totals, not per-file attribution",
            Color::Yellow,
        )
    };
    opts.stat(
        "attribution scale:",
        format!("{worst:.2} ({verdict})"),
        Some(color),
    )
}

/// The diff view: same dust-style renderer as the overview, but only the
/// diff's files — sized by REVIEW cost, with ΔC and status markers.
pub fn render_diff(report: &DiffReport, opts: Options) -> String {
    let total = report.totals.review_bytes.max(1) as f64;
    let entries = report
        .files
        .iter()
        .map(|f| entry(&f.path, f.review_bytes, f.new_lines, Some(f)));
    view(
        entries,
        total,
        opts,
        Some("REVIEW"),
        footer(opts, &report.version, None, Some(report)),
    )
}

/// The default view: one table merging the tree breakdown with the
/// diff's ΔC per touched path. Deleted files have no tree bytes but
/// their refunds still aggregate into their directory's ΔC.
pub fn render_overview(abs: &AbsReport, diff: &DiffReport, opts: Options) -> String {
    let mut changed: std::collections::HashMap<&str, &DiffFile> =
        diff.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut entries: Vec<Entry> = abs
        .files
        .iter()
        .map(|f| entry(&f.path, f.bytes, f.lines, changed.remove(f.path.as_str())))
        .collect();
    entries.extend(
        changed
            .into_values()
            .map(|c| entry(&c.path, 0.0, 0, Some(c))),
    );

    let total = abs.compressed_bytes.max(1) as f64;
    view(
        entries,
        total,
        opts,
        Some("BYTES"),
        footer(opts, &abs.version, Some(abs), Some(diff)),
    )
}

pub fn render_abs(report: &AbsReport, opts: Options) -> String {
    let total = report.compressed_bytes.max(1) as f64;
    let entries = report
        .files
        .iter()
        .map(|f| entry(&f.path, f.bytes, f.lines, None));
    view(
        entries,
        total,
        opts,
        None,
        footer(opts, &report.version, Some(report), None),
    )
}

fn fmt_bytes(bytes: f64) -> String {
    if bytes.abs() < 64.0 {
        "≈0".to_owned()
    } else if bytes.abs() < 1024.0 {
        format!("{bytes:.0} B")
    } else if bytes.abs() < 1024.0 * 1024.0 {
        format!("{:.1} KB", bytes / 1024.0)
    } else {
        format!("{:.1} MB", bytes / (1024.0 * 1024.0))
    }
}

fn fmt_signed(bytes: f64) -> String {
    if bytes.abs() < 64.0 {
        "≈0".to_owned()
    } else if bytes >= 0.0 {
        format!("+{}", fmt_bytes(bytes))
    } else {
        format!("−{}", fmt_bytes(-bytes))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pipeline::{AbsFile, Scales, Skipped, Totals};

    fn version() -> VersionInfo {
        VersionInfo {
            cx: "0",
            zstd: "1.5.7".into(),
            level: 19,
            max_window_log: 31,
        }
    }

    fn abs_report() -> AbsReport {
        AbsReport {
            version: version(),
            rev: "HEAD".into(),
            file_count: 2,
            raw_bytes: 40960,
            compressed_bytes: 10240,
            files: vec![
                AbsFile {
                    path: "src/a.rs".into(),
                    bytes: 8192.0,
                    lines: 200,
                },
                AbsFile {
                    path: "src/b.rs".into(),
                    bytes: 2048.0,
                    lines: 50,
                },
            ],
            scale: 0.95,
        }
    }

    fn diff_file(status: Status, density_outlier: bool) -> DiffFile {
        DiffFile {
            path: "src/a.rs".into(),
            status,
            review_bytes: 2048.0,
            review_raw: 4096,
            delta_bytes: 1024.0,
            new_lines: 40,
            bytes_per_line: None,
            density_outlier,
        }
    }

    fn diff_report() -> DiffReport {
        DiffReport {
            version: version(),
            base: "master".into(),
            merge_base: "abc123".into(),
            files: vec![diff_file(Status::Modified, false)],
            skipped: vec![Skipped {
                path: "Cargo.lock".into(),
                reason: "generated/vendored pattern".into(),
            }],
            totals: Totals {
                review_bytes: 2048,
                delta_bytes: 1024,
                added_lines: 40,
                deleted_lines: 12,
            },
            scales: Scales {
                review: 1.0,
                delta_new: 1.0,
                delta_old: 1.0,
            },
        }
    }

    const OPTS: Options = Options {
        top: 30,
        files: true,
        verbose: false,
        color: false,
    };
    const SUMMARY: &str =
        " C(tree) 10.0 KB   review 2.0 KB   ΔC +1.0 KB   lines +40 −12   1 skipped\n";

    /// Everything the overview prints below its table.
    fn footer(abs: &AbsReport, diff: &DiffReport, opts: Options) -> String {
        let rendered = render_overview(abs, diff, opts);
        rendered.rsplit("\n\n").next().unwrap().to_owned()
    }

    /// The numbers the run exists to give need no flag; nothing else
    /// comes without one.
    #[test]
    fn summary_line_is_the_whole_default_footer() {
        assert_eq!(footer(&abs_report(), &diff_report(), OPTS), SUMMARY);
    }

    #[test]
    fn verbose_adds_the_details_below_the_summary() {
        let opts = Options {
            verbose: true,
            ..OPTS
        };
        assert_eq!(
            footer(&abs_report(), &diff_report(), opts),
            format!(
                "{SUMMARY}\
                 \x20C(tree) over 2 files (40.0 KB raw)\n\
                 \x20skipped: Cargo.lock (generated/vendored pattern)\n\
                 \x20attribution scale: 0.95 (ok)   zstd 1.5.7, level 19, window≤2^31\n"
            )
        );
    }

    /// The gauge covers every pass the view merged, not just the diff's.
    #[test]
    fn attribution_gauge_reports_the_noisiest_scale() {
        let mut abs = abs_report();
        abs.scale = 0.4;
        let opts = Options {
            verbose: true,
            ..OPTS
        };
        let footer = footer(&abs, &diff_report(), opts);
        assert!(
            footer.contains("attribution scale: 0.40 (noisy"),
            "{footer}"
        );
    }

    #[test]
    fn no_files_leaves_the_summary_alone() {
        let opts = Options {
            files: false,
            ..OPTS
        };
        assert_eq!(
            render_overview(&abs_report(), &diff_report(), opts),
            SUMMARY
        );
    }

    /// Color is an input, not something the renderer discovers about the
    /// process it happens to run in.
    #[test]
    fn color_paints_labels_and_magnitudes() {
        let opts = Options {
            color: true,
            ..OPTS
        };
        let rendered = render_overview(&abs_report(), &diff_report(), opts);
        let grey = "\u{1b}[38;5;8m";
        let yellow = "\u{1b}[38;5;11m";
        let reset = "\u{1b}[0m";
        assert_eq!(
            rendered.rsplit("\n\n").next().unwrap(),
            format!(
                " {grey}C(tree){reset} 10.0 KB   \
                 {grey}review{reset} {yellow}2.0 KB{reset}   \
                 {grey}ΔC{reset} {yellow}+1.0 KB{reset}   \
                 {grey}lines{reset} +40 −12   \
                 {grey}1 skipped{reset}\n"
            )
        );
        assert!(rendered.starts_with(grey), "table header must color too");
    }

    /// An empty diff still reports C(tree): the run is not a no-op.
    #[test]
    fn empty_diff_says_so_in_place_of_the_totals() {
        let mut diff = diff_report();
        diff.files.clear();
        diff.skipped.clear();
        assert_eq!(
            footer(&abs_report(), &diff, OPTS),
            " C(tree) 10.0 KB   no scorable changes against master\n"
        );
    }

    #[test]
    fn status_markers() {
        let renamed = Status::Renamed {
            from: "a/b.rs".into(),
        };
        assert_eq!(
            status_marker(&diff_file(Status::Added, false)),
            Some("+".into())
        );
        assert_eq!(
            status_marker(&diff_file(Status::Deleted, false)),
            Some("−".into())
        );
        assert_eq!(
            status_marker(&diff_file(renamed, false)),
            Some("→ a/b.rs".into())
        );
        assert_eq!(status_marker(&diff_file(Status::Modified, false)), None);
        assert_eq!(
            status_marker(&diff_file(Status::Modified, true)),
            Some("⚠".into())
        );
        assert_eq!(
            status_marker(&diff_file(Status::Added, true)),
            Some("+ ⚠".into())
        );
    }

    #[test]
    fn byte_formatting() {
        assert_eq!(fmt_bytes(20.0), "≈0");
        assert_eq!(fmt_bytes(431.0), "431 B");
        assert_eq!(fmt_bytes(4300.8), "4.2 KB");
        assert_eq!(fmt_signed(4000.0), "+3.9 KB");
        assert_eq!(fmt_signed(-5120.0), "−5.0 KB");
        assert_eq!(fmt_signed(-12.0), "≈0");
    }
}
