//! Human-readable rendering via comfy-table: layout, color, and the
//! piped-output fallback (styling only applies on a tty) all come from
//! the library. The JSON form is serde on the report structs — that
//! serialization is the contract tooling consumes.

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets};

use crate::breakdown::{self, Entry, Node};
use crate::pipeline::{DiffReport, TreeReport};

/// One rendered line of the dust-style tree breakdown.
struct BreakdownRow {
    bytes: f64,
    delta: Option<f64>,
    marker: Option<String>,
    lines: Option<u64>,
    label: String,
    is_dir: bool,
    is_elision: bool,
}

/// Diff-status colors follow the universal diff convention.
fn marker_color(marker: &str) -> Option<Color> {
    if marker.contains('⚠') {
        Some(Color::Yellow)
    } else if marker.starts_with('+') {
        Some(Color::Green)
    } else if marker.starts_with('−') {
        Some(Color::Red)
    } else if marker.starts_with('→') {
        Some(Color::Cyan)
    } else {
        None
    }
}

impl BreakdownRow {
    fn into_cells(self, total: f64, show_delta: bool) -> Vec<Cell> {
        let share = 100.0 * self.bytes / total;
        let filled = ((share / 10.0).round() as usize).min(10);
        let bar = format!(
            "{}{}  {share:>4.1}%",
            "█".repeat(filled),
            "░".repeat(10 - filled)
        );
        let dim = self.is_elision.then_some(Color::DarkGrey);
        let path_cell = if self.is_elision {
            Cell::new(self.label).fg(Color::DarkGrey)
        } else if self.is_dir {
            Cell::new(self.label).add_attribute(Attribute::Bold)
        } else {
            Cell::new(self.label)
        };
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
            cells.push(match color {
                Some(c) => Cell::new(marker).fg(c),
                None => Cell::new(marker),
            });
        }
        cells.extend([
            num_cell(self.lines.map_or("-".to_owned(), |l| l.to_string()), dim),
            path_cell,
            num_cell(bar, dim.or((share >= 25.0).then_some(Color::Yellow))),
        ]);
        cells
    }
}

/// Walk the pruned tree into rows, biggest first within each directory,
/// with an elision summary as the last child where pruning bit.
fn collect_rows(node: &Node, prefix: &str, out: &mut Vec<BreakdownRow>) {
    let child_count = node.children.len() + usize::from(node.elided_count > 0);
    for (i, child) in node.children.iter().enumerate() {
        let is_last = i + 1 == child_count;
        let connector = if is_last { "└─" } else { "├─" };
        let tip = if child.children.is_empty() && child.elided_count == 0 {
            "─ "
        } else {
            "┬ "
        };
        out.push(BreakdownRow {
            bytes: child.bytes,
            delta: child.delta,
            marker: child.marker.clone(),
            lines: Some(child.lines),
            label: format!("{prefix}{connector}{tip}{}", child.name),
            is_dir: child.is_dir,
            is_elision: false,
        });
        let child_prefix = format!("{prefix}{}", if is_last { "  " } else { "│ " });
        collect_rows(child, &child_prefix, out);
    }
    if node.elided_count > 0 {
        out.push(BreakdownRow {
            bytes: node.elided_bytes,
            delta: node.elided_delta,
            marker: None,
            lines: None,
            label: format!("{prefix}└── … +{} more", node.elided_count),
            is_dir: false,
            is_elision: true,
        });
    }
}

/// Render entries as the dust-style table. `bytes_header` names the size
/// column ("BYTES" for tree contributions, "REVIEW" for diff cost);
/// `Some` adds the diff columns (ΔC + status marker), `None` omits them.
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
    let mut table = table_with_header(columns);
    let mut rows = Vec::new();
    collect_rows(&root, "", &mut rows);
    for row in rows {
        table.add_row(row.into_cells(total, diff_columns.is_some()));
    }
    table.to_string()
}

fn table_with_header(columns: &[&str]) -> Table {
    let mut table = Table::new();
    table.load_preset(presets::NOTHING);
    table.set_content_arrangement(ContentArrangement::Dynamic);
    table.set_header(columns.iter().map(|c| Cell::new(c).fg(Color::DarkGrey)));
    table
}

fn num_cell(text: String, color: Option<Color>) -> Cell {
    let cell = Cell::new(text).set_alignment(CellAlignment::Right);
    match color {
        Some(c) => cell.fg(c),
        None => cell,
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

/// Diff-status indicator per the JSON status strings: "+" added,
/// "−" deleted, "→ <from>" renamed, "⚠" appended for density outliers.
fn status_marker(file: &crate::pipeline::DiffFile) -> Option<String> {
    let base = match file.status.as_str() {
        "added" => Some("+".to_owned()),
        "deleted" => Some("−".to_owned()),
        s => s
            .strip_prefix("renamed from ")
            .map(|from| format!("→ {from}")),
    };
    if file.density_outlier {
        let base = base.unwrap_or_default();
        Some(format!("{base} ⚠").trim().to_owned())
    } else {
        base
    }
}

/// The diff view: same dust-style renderer as the overview, but only the
/// diff's files — sized by REVIEW cost, with ΔC and status markers.
pub fn render_diff(report: &DiffReport, top: usize) -> String {
    let mut out = String::new();
    if report.files.is_empty() && report.skipped.is_empty() {
        return format!("no scorable changes against {}\n", report.base);
    }

    let total = report.totals.review_bytes.max(1) as f64;
    let entries = report.files.iter().map(|f| Entry {
        path: &f.path,
        bytes: f.review_bytes,
        lines: f.new_lines,
        delta: Some(f.delta_bytes),
        marker: status_marker(f),
    });
    out.push_str(&breakdown_table(entries, total, top, Some("REVIEW")));
    out.push('\n');

    out.push_str(&format!(
        "\n PR total: review {}, ΔC {}\n",
        fmt_bytes(report.totals.review_bytes as f64),
        fmt_signed(report.totals.delta_bytes as f64),
    ));
    let worst = worst_scale(report, 1.0);
    let verdict = if (0.7..=1.1).contains(&worst) {
        "ok"
    } else {
        "noisy — trust totals, not per-file attribution"
    };
    out.push_str(&format!(
        " attribution scale: {worst:.2} ({verdict})   {}\n",
        version_line(report),
    ));
    if !report.skipped.is_empty() {
        let list: Vec<String> = report
            .skipped
            .iter()
            .map(|s| format!("{} ({})", s.path, s.reason))
            .collect();
        out.push_str(&format!(" skipped: {}\n", list.join(", ")));
    }
    out
}

/// Entries for the tree-only view: no diff information.
fn tree_entries(report: &TreeReport) -> impl Iterator<Item = Entry<'_>> {
    report.files.iter().map(|f| Entry {
        path: &f.path,
        bytes: f.bytes,
        lines: f.lines,
        delta: None,
        marker: None,
    })
}

/// The default view: one table merging the tree breakdown with the
/// diff's ΔC per touched path. Deleted files have no tree bytes but
/// their refunds still aggregate into their directory's ΔC.
pub fn render_overview(tree: &TreeReport, diff: &DiffReport, top: usize) -> String {
    let mut changed: std::collections::HashMap<&str, &crate::pipeline::DiffFile> =
        diff.files.iter().map(|f| (f.path.as_str(), f)).collect();
    let mut entries: Vec<Entry> = tree
        .files
        .iter()
        .map(|f| {
            let change = changed.remove(f.path.as_str());
            Entry {
                path: &f.path,
                bytes: f.bytes,
                lines: f.lines,
                delta: change.map(|c| c.delta_bytes),
                marker: change.and_then(status_marker),
            }
        })
        .collect();
    entries.extend(changed.into_values().map(|c| Entry {
        path: &c.path,
        bytes: 0.0,
        lines: 0,
        delta: Some(c.delta_bytes),
        marker: status_marker(c),
    }));

    let total = tree.compressed_bytes.max(1) as f64;
    let mut out = breakdown_table(entries, total, top, Some("BYTES"));
    out.push_str("\n\n");
    out.push_str(&format!(
        " C(tree) = {} over {} files ({} raw)\n",
        fmt_bytes(tree.compressed_bytes as f64),
        tree.file_count,
        fmt_bytes(tree.raw_bytes as f64),
    ));
    if diff.files.is_empty() && diff.skipped.is_empty() {
        out.push_str(&format!(" no scorable changes against {}\n", diff.base));
    } else {
        out.push_str(&format!(
            " PR total: review {}, ΔC {}\n",
            fmt_bytes(diff.totals.review_bytes as f64),
            fmt_signed(diff.totals.delta_bytes as f64),
        ));
    }
    let worst = worst_scale(diff, tree.scale);
    let verdict = if (0.7..=1.1).contains(&worst) {
        "ok"
    } else {
        "noisy — trust totals, not per-file attribution"
    };
    out.push_str(&format!(
        " attribution scale: {worst:.2} ({verdict})   {}\n",
        version_line(diff),
    ));
    if !diff.skipped.is_empty() {
        let list: Vec<String> = diff
            .skipped
            .iter()
            .map(|s| format!("{} ({})", s.path, s.reason))
            .collect();
        out.push_str(&format!(" skipped: {}\n", list.join(", ")));
    }
    out
}

pub fn render_tree(report: &TreeReport, top: usize) -> String {
    let mut out = String::new();
    if !report.files.is_empty() {
        let total = report.compressed_bytes.max(1) as f64;
        out.push_str(&breakdown_table(tree_entries(report), total, top, None));
        out.push_str("\n\n");
    }
    out.push_str(&format!(
        " C(tree) = {} over {} files ({} raw)",
        fmt_bytes(report.compressed_bytes as f64),
        report.file_count,
        fmt_bytes(report.raw_bytes as f64),
    ));
    if report.files.is_empty() {
        out.push_str(&format!(
            "   zstd {}, level {}\n",
            report.version.zstd, report.version.level
        ));
    } else {
        out.push_str(&format!(
            "\n attribution scale: {:.2}   zstd {}, level {}, window≤2^{}\n",
            report.scale, report.version.zstd, report.version.level, report.version.max_window_log,
        ));
    }
    out
}

/// The scale farthest from 1.0 across the diff's three gauges plus one
/// extra (the tree's, for the overview; pass 1.0 to ignore).
fn worst_scale(report: &DiffReport, extra: f64) -> f64 {
    [
        report.scales.review,
        report.scales.delta_new,
        report.scales.delta_old,
        extra,
    ]
    .into_iter()
    .fold(1.0f64, |acc, s| {
        if (s - 1.0).abs() > (acc - 1.0).abs() {
            s
        } else {
            acc
        }
    })
}

fn version_line(report: &DiffReport) -> String {
    format!(
        "zstd {}, level {}, window≤2^{}",
        report.version.zstd, report.version.level, report.version.max_window_log
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
        let file = |status: &str, outlier| crate::pipeline::DiffFile {
            path: "x".into(),
            status: status.into(),
            review_bytes: 0.0,
            review_raw: 0,
            delta_bytes: 0.0,
            new_lines: 0,
            bytes_per_line: None,
            density_outlier: outlier,
        };
        assert_eq!(status_marker(&file("added", false)), Some("+".into()));
        assert_eq!(status_marker(&file("deleted", false)), Some("−".into()));
        assert_eq!(
            status_marker(&file("renamed from a/b.rs", false)),
            Some("→ a/b.rs".into())
        );
        assert_eq!(status_marker(&file("modified", false)), None);
        assert_eq!(status_marker(&file("modified", true)), Some("⚠".into()));
        assert_eq!(status_marker(&file("added", true)), Some("+ ⚠".into()));
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
