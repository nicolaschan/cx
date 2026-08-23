//! Human-readable rendering. The JSON form is just serde on the report
//! structs — that serialization is the contract the agent skill consumes.

use crate::pipeline::{ScoreReport, TreeReport};

pub fn render_score(report: &ScoreReport) -> String {
    let mut out = String::new();
    if report.files.is_empty() && report.skipped.is_empty() {
        return format!("no scorable changes against {}\n", report.base);
    }

    out.push_str(" REVIEW    ΔCOMPLEX    B/LINE   PATH\n");
    for f in &report.files {
        let annotation = match f.status.as_str() {
            "modified" => String::new(),
            s => format!(" ({s})"),
        };
        let flag = if f.density_outlier {
            "  ⚠ density outlier"
        } else {
            ""
        };
        out.push_str(&format!(
            " {:>7}  {:>9}  {:>7}   {}{}{}\n",
            fmt_bytes(f.review_bytes),
            fmt_signed(f.delta_bytes),
            f.bytes_per_line
                .map_or("-".to_owned(), |d| format!("{d:.0}")),
            f.path,
            annotation,
            flag,
        ));
    }

    out.push_str("──────────────────────────────────────────────\n");
    out.push_str(&format!(
        " PR total: review {}, Δcomplexity {}\n",
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
        " attribution scale: {:.2} ({})   zstd {}, level {}, window≤2^{}\n",
        worst_scale,
        verdict,
        report.version.zstd,
        report.version.level,
        report.version.max_window_log,
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

pub fn render_tree(report: &TreeReport) -> String {
    format!(
        "C(tree) = {} over {} files ({} raw)   zstd {}, level {}\n",
        fmt_bytes(report.compressed_bytes as f64),
        report.files,
        fmt_bytes(report.raw_bytes as f64),
        report.version.zstd,
        report.version.level,
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
