use std::io::IsTerminal;

use anyhow::{Result, bail};
use clap::builder::BoolishValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

use cx_cli::git::{Git, Side};
use cx_cli::pipeline::{self, AbsOptions, DiffOptions};
use cx_cli::progress::Progress;
use cx_cli::report;

/// Score git trees and diffs by marginal description length: how much
/// new information content adds, conditioned on what the codebase
/// already contains. With no subcommand, shows one merged breakdown:
/// the tree's complexity per file/directory plus the diff's ΔCX.
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

/// Flags every view accepts, declared once so a default pinned through
/// the environment means the same thing in all of them.
#[derive(Args)]
struct CommonArgs {
    /// Score the index: staged changes only.
    #[arg(long, conflicts_with = "committed")]
    staged: bool,
    /// Score HEAD: committed code only, ignoring the index and the
    /// working tree.
    #[arg(long)]
    committed: bool,
    /// Score test files too. They are excluded by default, by naming
    /// convention. Takes an optional value so a pinned default can be
    /// vetoed for one run: `--include-tests=false`.
    #[arg(
        long,
        env = "CX_INCLUDE_TESTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    include_tests: bool,
    /// Score comments too. By default every file is reduced to code —
    /// comments stripped, blank lines dropped — before scoring. Takes an
    /// optional value so a pinned default can be vetoed for one run:
    /// `--comments=false`.
    #[arg(
        long,
        env = "CX_COMMENTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    comments: bool,
    /// Score prose files too — Markdown, reStructuredText, plain text,
    /// AsciiDoc, Org, and extensionless documents such as LICENSE. By
    /// default they are skipped. Takes an optional value so a pinned
    /// default can be vetoed for one run: `--prose=false`.
    #[arg(
        long,
        env = "CX_PROSE",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    prose: bool,
    /// Hide the per-file breakdown; show the summary line only.
    #[arg(long)]
    no_files: bool,
    /// Show only the N biggest files/directories in the breakdown.
    #[arg(short = 'n', long, env = "CX_TOP", default_value_t = 30)]
    top: usize,
    /// Also show compressor provenance and the skipped files by name.
    #[arg(short = 'v', long)]
    verbose: bool,
    #[arg(long)]
    json: bool,
}

/// Flags that select what to diff.
#[derive(Args)]
struct DiffArgs {
    /// Base ref to diff against (default: main/master merge-base).
    #[arg(long, env = "CX_BASE")]
    base: Option<String>,
}

impl DiffArgs {
    fn options(self, common: &CommonArgs) -> DiffOptions {
        DiffOptions {
            base: self.base,
            side: common.side(),
            include_tests: common.include_tests,
            comments: common.comments,
            prose: common.prose,
        }
    }
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

    fn abs_options(&self) -> AbsOptions {
        AbsOptions {
            include_tests: self.include_tests,
            comments: self.comments,
            prose: self.prose,
            side: self.side(),
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
        #[command(flatten)]
        diff: DiffArgs,
        #[arg(long, value_enum, default_value = "file")]
        granularity: Granularity,
        #[command(flatten)]
        common: CommonArgs,
    },
    /// Absolute C(tree) — the trend-line number.
    Abs {
        #[command(flatten)]
        common: CommonArgs,
    },
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
    match cli.cmd {
        None => {
            let common = cli.common;
            let abs = pipeline::abs(&git, &common.abs_options(), progress)?;
            let diff = pipeline::diff(&git, &cli.diff.options(&common), progress)?;
            emit(
                common.json,
                &serde_json::json!({ "abs": abs, "diff": diff }),
                report::render_overview(&abs, &diff, common.report_options()),
            )?;
        }
        Some(Cmd::Diff {
            diff,
            granularity,
            common,
        }) => {
            if matches!(granularity, Granularity::Hunk) {
                bail!("hunk granularity is not implemented yet (plan phase 3)");
            }
            let report = pipeline::diff(&git, &diff.options(&common), progress)?;
            emit(
                common.json,
                &report,
                report::render_diff(&report, common.report_options()),
            )?;
        }
        Some(Cmd::Abs { common }) => {
            let report = pipeline::abs(&git, &common.abs_options(), progress)?;
            emit(
                common.json,
                &report,
                report::render_abs(&report, common.report_options()),
            )?;
        }
    }
    Ok(())
}
