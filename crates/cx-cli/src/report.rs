//! Human-readable rendering via comfy-table: layout, color, and the
//! piped-output fallback (styling only applies on a tty) all come from
//! the library. The JSON form is serde on the report structs — that
//! serialization is the contract tooling consumes.

use comfy_table::{Attribute, Cell, CellAlignment, Color, ContentArrangement, Table, presets};

use crate::breakdown::{self, Node};
use crate::pipeline::{ScoreReport, TreeReport};

/// One rendered line of the dust-style tree breakdown.
struct BreakdownRow {
    bytes: f64,
    lines: Option<u64>,
    label: String,
    is_dir: bool,
    is_elision: bool,
}

impl BreakdownRow {
    fn into_cells(self, total: f64) -> Vec<Cell> {
        let share = 100.0 * self.bytes / total;
        let filled = ((share / 10.0).round() as usize).min(10);
        let bar = format!(
            "{}{}  {share:>4.1}%",
            "█".repeat(filled),
            "░".repeat(10 - filled)
        );
        if self.is_elision {
            return vec![
                num_cell(fmt_bytes(self.bytes), Some(Color::DarkGrey)),
                num_cell("-".to_owned(), Some(Color::DarkGrey)),
                Cell::new(self.label).fg(Color::DarkGrey),
                num_cell(bar, Some(Color::DarkGrey)),
            ];
        }
        let path_cell = if self.is_dir {
            Cell::new(self.label).add_attribute(Attribute::Bold)
        } else {
            Cell::new(self.label)
        };
        vec![
            num_cell(fmt_bytes(self.bytes), score_color(self.bytes)),
            num_cell(self.lines.map_or("-".to_owned(), |l| l.to_string()), None),
            path_cell,
            num_cell(bar, (share >= 25.0).then_some(Color::Yellow)),
        ]
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
            lines: None,
            label: format!("{prefix}└── … +{} more", node.elided_count),
            is_dir: false,
            is_elision: true,
        });
    }
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

pub fn render_score(report: &ScoreReport) -> String {
    let mut out = String::new();
    if report.files.is_empty() && report.skipped.is_empty() {
        return format!("no scorable changes against {}\n", report.base);
    }

    let mut table = table_with_header(&["REVIEW", "ΔC", "B/LINE", "PATH", ""]);
    for f in &report.files {
        let mut notes: Vec<String> = Vec::new();
        if f.status != "modified" {
            notes.push(format!("({})", f.status));
        }
        if f.density_outlier {
            notes.push("⚠ density outlier".to_owned());
        }
        let note_color = if f.density_outlier {
            Color::Yellow
        } else {
            Color::DarkGrey
        };
        table.add_row(vec![
            num_cell(fmt_bytes(f.review_bytes), score_color(f.review_bytes)),
            num_cell(fmt_signed(f.delta_bytes), score_color(f.delta_bytes)),
            num_cell(
                f.bytes_per_line
                    .map_or("-".to_owned(), |d| format!("{d:.0}")),
                None,
            ),
            Cell::new(&f.path),
            Cell::new(notes.join("  ")).fg(note_color),
        ]);
    }
    out.push_str(&table.to_string());
    out.push('\n');

    out.push_str(&format!(
        "\n PR total: review {}, ΔC {}\n",
        fmt_bytes(report.totals.review_bytes as f64),
        fmt_signed(report.totals.delta_bytes as f64),
    ));
    let worst_scale = [
        report.scales.review,
        report.scales.delta_new,
        report.scales.delta_old,
    ]
    .into_iter()
    .fold(1.0f64, |acc, s| {
        if (s - 1.0).abs() > (acc - 1.0).abs() {
            s
        } else {
            acc
        }
    });
    let verdict = if (0.7..=1.1).contains(&worst_scale) {
        "ok"
    } else {
        "noisy — trust totals, not per-file attribution"
    };
    out.push_str(&format!(
        " attribution scale: {:.2} ({})   {}\n",
        worst_scale,
        verdict,
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

pub fn render_tree(report: &TreeReport, top: usize) -> String {
    let mut out = String::new();
    if !report.files.is_empty() {
        let total = report.compressed_bytes.max(1) as f64;
        let root = breakdown::breakdown(&report.files, top);
        let mut table = table_with_header(&["BYTES", "LINES", "PATH", "SHARE"]);
        let mut rows = Vec::new();
        collect_rows(&root, "", &mut rows);
        for row in rows {
            table.add_row(row.into_cells(total));
        }
        out.push_str(&table.to_string());
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

fn version_line(report: &ScoreReport) -> String {
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
    fn byte_formatting() {
        assert_eq!(fmt_bytes(20.0), "≈0");
        assert_eq!(fmt_bytes(431.0), "431 B");
        assert_eq!(fmt_bytes(4300.8), "4.2 KB");
        assert_eq!(fmt_signed(4000.0), "+3.9 KB");
        assert_eq!(fmt_signed(-5120.0), "−5.0 KB");
        assert_eq!(fmt_signed(-12.0), "≈0");
    }
}
