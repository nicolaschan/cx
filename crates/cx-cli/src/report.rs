//! Human-readable rendering via comfy-table: layout, color, and the
//! piped-output fallback (styling only applies on a tty) all come from
//! the library. The JSON form is serde on the report structs — that
//! serialization is the contract tooling consumes.

use std::io::IsTerminal;
use std::sync::OnceLock;

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets};
use crossterm::style::{ResetColor, SetForegroundColor};

use crate::breakdown::{self, Entry, Node};
use crate::git::Status;
use crate::pipeline::{AbsReport, DiffFile, DiffReport};

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

/// Whether to emit escape sequences, decided exactly as comfy-table
/// decides it for the tables — so a run either colors everything or
/// nothing, and piped output stays plain.
fn styling_enabled() -> bool {
    static TTY: OnceLock<bool> = OnceLock::new();
    *TTY.get_or_init(|| std::io::stdout().is_terminal())
}

/// Color a footer line's fragment. Colors come from the same
/// `comfy_table::Color` vocabulary the table cells use (the crate
/// re-exports crossterm's type), so a value means the same thing above
/// and below the table.
fn paint(text: impl std::fmt::Display, color: Option<Color>) -> String {
    match color.filter(|_| styling_enabled()) {
        Some(c) => format!("{}{text}{}", SetForegroundColor(c), ResetColor),
        None => text.to_string(),
    }
}

/// Provenance and other incidental metadata: present, never competing
/// with the numbers.
fn dim(text: impl std::fmt::Display) -> String {
    paint(text, Some(Color::DarkGrey))
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

/// Render entries as the dust-style table. `diff_columns` names the size
/// column ("BYTES" for tree contributions, "REVIEW" for diff cost) and
/// adds the ΔC + status columns; `None` renders the plain tree view.
fn breakdown_table<'a>(
    entries: impl IntoIterator<Item = Entry<'a>>,
    total: f64,
    top: usize,
    diff_columns: Option<&'static str>,
) -> String {
    let root = breakdown::breakdown(entries, top);
    let columns: &[&str] = match diff_columns {
        Some(bytes_header) => &[bytes_header, "ΔC", "", "LINES", "PATH", "SHARE"],
        None => &["BYTES", "LINES", "PATH", "SHARE"],
    };
    let mut table = Table::new();
    table.load_preset(presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(columns.iter().map(|c| Cell::new(c).fg(Color::DarkGrey)));
    push_children(&mut table, &root, "", total, diff_columns.is_some());
    table.to_string()
}

/// The footer both diff-aware views share: PR total (or "no changes"),
/// the attribution-scale verdict, and the skipped list.
fn diff_footer(diff: &DiffReport, extra_scale: f64) -> String {
    let mut out = String::new();
    if diff.files.is_empty() && diff.skipped.is_empty() {
        out.push_str(&dim(format!(" no scorable changes against {}", diff.base)));
        out.push('\n');
    } else {
        // The totals carry the same magnitude coloring as the cells they
        // sum, so a red total and a red row mean one thing.
        let review = diff.totals.review_bytes as f64;
        let delta = diff.totals.delta_bytes as f64;
        out.push_str(&format!(
            " PR total: review {}, ΔC {}\n",
            paint(fmt_bytes(review), score_color(review)),
            paint(fmt_signed(delta), score_color(delta)),
        ));
    }
    let worst = [
        diff.scales.review,
        diff.scales.delta_new,
        diff.scales.delta_old,
        extra_scale,
    ]
    .into_iter()
    .fold(1.0f64, |acc, s| {
        if (s - 1.0).abs() > (acc - 1.0).abs() {
            s
        } else {
            acc
        }
    });
    out.push_str(&format!(
        " {} {}   {}\n",
        dim("attribution scale:"),
        scale_gauge(worst),
        provenance_line(&diff.version),
    ));
    if !diff.skipped.is_empty() {
        let list: Vec<String> = diff
            .skipped
            .iter()
            .map(|s| format!("{} ({})", s.path, s.reason))
            .collect();
        out.push_str(&dim(format!(" skipped: {}", list.join(", "))));
        out.push('\n');
    }
    out
}

/// The attribution noise gauge, colored by whether per-item numbers can
/// be trusted at all. One definition of "trustworthy" for every view.
fn scale_gauge(scale: f64) -> String {
    let trustworthy = (0.7..=1.1).contains(&scale);
    let (verdict, color) = if trustworthy {
        ("ok", Color::Green)
    } else {
        (
            "noisy — trust totals, not per-file attribution",
            Color::Yellow,
        )
    };
    paint(format!("{scale:.2} ({verdict})"), Some(color))
}

/// Compressor provenance: the scores mean nothing without it, but it
/// never changes run to run — dim.
fn provenance_line(version: &crate::pipeline::VersionInfo) -> String {
    dim(format!(
        "zstd {}, level {}, window≤2^{}",
        version.zstd, version.level, version.max_window_log
    ))
}

/// C(tree) is a whole-repo absolute, not a change: no magnitude color
/// (it would sit permanently red), just the count and raw size dimmed.
fn ctree_line(abs: &AbsReport) -> String {
    format!(
        " C(tree) = {} {}",
        fmt_bytes(abs.compressed_bytes as f64),
        dim(format!(
            "over {} files ({} raw)",
            abs.file_count,
            fmt_bytes(abs.raw_bytes as f64),
        )),
    )
}

/// The diff view: same dust-style renderer as the overview, but only the
/// diff's files — sized by REVIEW cost, with ΔC and status markers.
pub fn render_diff(report: &DiffReport, top: usize) -> String {
    if report.files.is_empty() && report.skipped.is_empty() {
        return format!(
            "{}\n",
            dim(format!("no scorable changes against {}", report.base))
        );
    }
    let total = report.totals.review_bytes.max(1) as f64;
    let entries = report
        .files
        .iter()
        .map(|f| entry(&f.path, f.review_bytes, f.new_lines, Some(f)));
    let table = breakdown_table(entries, total, top, Some("REVIEW"));
    format!("{table}\n\n{}", diff_footer(report, 1.0))
}

/// The default view: one table merging the tree breakdown with the
/// diff's ΔC per touched path. Deleted files have no tree bytes but
/// their refunds still aggregate into their directory's ΔC.
pub fn render_overview(abs: &AbsReport, diff: &DiffReport, top: usize) -> String {
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
    let table = breakdown_table(entries, total, top, Some("BYTES"));
    format!(
        "{table}\n\n{}\n{}",
        ctree_line(abs),
        diff_footer(diff, abs.scale)
    )
}

pub fn render_abs(report: &AbsReport, top: usize) -> String {
    if report.files.is_empty() {
        return format!(
            "{}   {}\n",
            ctree_line(report),
            provenance_line(&report.version)
        );
    }
    let total = report.compressed_bytes.max(1) as f64;
    let entries = report
        .files
        .iter()
        .map(|f| entry(&f.path, f.bytes, f.lines, None));
    let table = breakdown_table(entries, total, top, None);
    format!(
        "{table}\n\n{}\n {} {}   {}\n",
        ctree_line(report),
        dim("attribution scale:"),
        scale_gauge(report.scale),
        provenance_line(&report.version),
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

    #[test]
    fn status_markers() {
        let file = |status: Status, outlier| DiffFile {
            path: "x".into(),
            status,
            review_bytes: 0.0,
            review_raw: 0,
            delta_bytes: 0.0,
            new_lines: 0,
            bytes_per_line: None,
            density_outlier: outlier,
        };
        let renamed = Status::Renamed {
            from: "a/b.rs".into(),
        };
        assert_eq!(status_marker(&file(Status::Added, false)), Some("+".into()));
        assert_eq!(
            status_marker(&file(Status::Deleted, false)),
            Some("−".into())
        );
        assert_eq!(
            status_marker(&file(renamed, false)),
            Some("→ a/b.rs".into())
        );
        assert_eq!(status_marker(&file(Status::Modified, false)), None);
        assert_eq!(
            status_marker(&file(Status::Modified, true)),
            Some("⚠".into())
        );
        assert_eq!(
            status_marker(&file(Status::Added, true)),
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
