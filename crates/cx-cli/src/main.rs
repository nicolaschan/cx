use std::io::IsTerminal;

use anyhow::{Result, bail};
use clap::builder::BoolishValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

use cx_cli::git::{Git, Side};
use cx_cli::pipeline::{self, Options};
use cx_cli::progress::Progress;
use cx_cli::report;
use cx_cli::strip::Keep;

/// Score git trees and diffs by marginal description length: how much
/// new information content adds, conditioned on what the codebase
/// already contains. With no subcommand, shows one merged breakdown:
/// the tree's complexity per file/directory plus the diff's ΔCX.
///
/// A run measures the directory it is started in — the whole repository
/// at its root, and inside a subdirectory that subtree as its own
/// codebase, with paths named from there.
#[derive(Parser)]
#[command(name = "cx", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
    #[command(flatten)]
    diff: DiffArgs,
    #[command(flatten)]
    common: CommonArgs,
}

/// Flags every view accepts, declared once — globally, so a flag means
/// the same thing wherever it appears, before or after the subcommand.
#[derive(Args)]
struct CommonArgs {
    /// Score the index: staged changes only.
    #[arg(long, global = true, conflicts_with = "committed")]
    staged: bool,
    /// Score HEAD: committed code only, ignoring the index and the
    /// working tree.
    #[arg(long, global = true)]
    committed: bool,
    /// Score test files too. They are excluded by default, by naming
    /// convention. Takes an optional value so a pinned default can be
    /// vetoed for one run: `--include-tests=false`.
    #[arg(
        long,
        global = true,
        env = "CX_INCLUDE_TESTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    include_tests: bool,
    /// Score comments too. By default every file is reduced to code —
    /// comments stripped, string contents emptied, blank lines dropped —
    /// before scoring. Takes an optional value so a pinned default can
    /// be vetoed for one run: `--comments=false`.
    #[arg(
        long,
        global = true,
        env = "CX_COMMENTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    comments: bool,
    /// Score string literal contents too. By default a string counts —
    /// its delimiters stay — but its contents are emptied before
    /// scoring, like comments. Takes an optional value so a pinned
    /// default can be vetoed for one run: `--strings=false`.
    #[arg(
        long,
        global = true,
        env = "CX_STRINGS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    strings: bool,
    /// Score prose files too — Markdown, reStructuredText, plain text,
    /// AsciiDoc, Org, and extensionless documents such as LICENSE. By
    /// default they are skipped. Takes an optional value so a pinned
    /// default can be vetoed for one run: `--prose=false`.
    #[arg(
        long,
        global = true,
        env = "CX_PROSE",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    prose: bool,
    /// Score data files too — JSON, XML, SVG, and the tabular or
    /// line-delimited formats (CSV, TSV, JSON Lines, GeoJSON). By
    /// default they are skipped, like prose. Takes an optional value so
    /// a pinned default can be vetoed for one run: `--data=false`.
    #[arg(
        long,
        global = true,
        env = "CX_DATA",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    data: bool,
    /// Restrict the run to paths matching GLOB, read from the directory
    /// cx runs in. Repeatable; gitignore syntax, `!` excludes, and among
    /// globs the last match wins. Paths outside the scope are not scored
    /// and not part of the reference — `-g 'crates/api/**'` sizes that
    /// subtree as its own codebase, as running inside it does.
    #[arg(
        short = 'g',
        long = "glob",
        global = true,
        env = "CX_GLOB",
        value_name = "GLOB"
    )]
    globs: Vec<String>,
    /// Hide the per-file breakdown; show the summary line only.
    #[arg(long, global = true)]
    no_files: bool,
    /// Show only the N biggest files/directories in the breakdown.
    #[arg(short = 'n', long, global = true, env = "CX_TOP", default_value_t = 30)]
    top: usize,
    /// Also show compressor provenance and the skipped files by name.
    #[arg(short = 'v', long, global = true)]
    verbose: bool,
    #[arg(long, global = true)]
    json: bool,
}

/// Flags that select what to diff.
#[derive(Args)]
struct DiffArgs {
    /// Base ref to diff against (default: main/master merge-base).
    #[arg(long, global = true, env = "CX_BASE")]
    base: Option<String>,
}

impl CommonArgs {
    fn side(&self) -> Side {
        if self.committed {
            Side::Head
        } else if self.staged {
            Side::Index
        } else {
            Side::Worktree
        }
    }

    fn options(&self) -> Options {
        Options {
            side: self.side(),
            include_tests: self.include_tests,
            keep: Keep {
                comments: self.comments,
                strings: self.strings,
            },
            prose: self.prose,
            data: self.data,
            globs: self.globs.clone(),
        }
    }

    fn report_options(&self) -> report::Options {
        report::Options {
            top: self.top,
            files: !self.no_files,
            verbose: self.verbose,
            color: std::io::stdout().is_terminal(),
        }
    }
}

#[derive(Subcommand)]
enum Cmd {
    /// Score the changes between a base branch and the working tree, the
    /// index, or HEAD.
    Diff {
        #[arg(long, value_enum, default_value = "file")]
        granularity: Granularity,
    },
    /// Absolute C(tree) — the trend-line number.
    Abs,
}

#[derive(Clone, Copy, ValueEnum)]
enum Granularity {
    File,
    Hunk,
}

/// Print a report as pretty JSON or its rendered table.
fn emit<T: serde::Serialize>(json: bool, value: &T, rendered: String) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    } else {
        print!("{rendered}");
    }
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let git = Git::discover()?;
    let progress = Progress {
        visible: std::io::stderr().is_terminal(),
    };
    let common = cli.common;
    let base = cli.diff.base.as_deref();
    match cli.cmd {
        None => {
            let opts = common.options();
            let abs = pipeline::abs(&git, &opts, progress)?;
            let diff = pipeline::diff(&git, base, &opts, progress)?;
            emit(
                common.json,
                &serde_json::json!({ "abs": abs, "diff": diff }),
                report::render_overview(&abs, &diff, common.report_options()),
            )?;
        }
        Some(Cmd::Diff { granularity }) => {
            if matches!(granularity, Granularity::Hunk) {
                bail!("hunk granularity is not implemented yet (plan phase 3)");
            }
            let report = pipeline::diff(&git, base, &common.options(), progress)?;
            emit(
                common.json,
                &report,
                report::render_diff(&report, common.report_options()),
            )?;
        }
        Some(Cmd::Abs) => {
            let report = pipeline::abs(&git, &common.options(), progress)?;
            emit(
                common.json,
                &report,
                report::render_abs(&report, common.report_options()),
            )?;
        }
    }
    Ok(())
}
