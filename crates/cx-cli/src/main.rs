use std::io::IsTerminal;

use anyhow::{Result, bail};
use clap::builder::BoolishValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

use cx_cli::git::Git;
use cx_cli::pipeline::{self, AbsOptions, DiffOptions};
use cx_cli::progress::Progress;
use cx_cli::report;

/// Score git trees and diffs by marginal description length: how much
/// new information content adds, conditioned on what the codebase
/// already contains. With no subcommand, shows one merged breakdown:
/// the tree's complexity per file/directory plus the diff's ΔC.
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
    /// Exclude test files everywhere, by naming convention. Takes an
    /// optional value so a pinned default can be vetoed for one run:
    /// `--ignore-tests=false`.
    #[arg(
        long,
        env = "CX_IGNORE_TESTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    ignore_tests: bool,
    /// Hide the per-file breakdown; show the summary line only.
    #[arg(long)]
    no_files: bool,
    /// Show only the N biggest files/directories in the breakdown.
    #[arg(short = 'n', long, env = "CX_TOP", default_value_t = 30)]
    top: usize,
    /// Also show attribution scale, compressor provenance, and the
    /// skipped files by name.
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
    /// Diff the index instead of HEAD.
    #[arg(long)]
    staged: bool,
}

impl DiffArgs {
    fn options(self, common: &CommonArgs) -> DiffOptions {
        DiffOptions {
            base: self.base,
            staged: self.staged,
            ignore_tests: common.ignore_tests,
        }
    }
}

impl CommonArgs {
    fn abs_options(&self) -> AbsOptions {
        AbsOptions {
            no_files: self.no_files,
            ignore_tests: self.ignore_tests,
        }
    }

    fn progress(&self) -> Progress {
        Progress {
            visible: std::io::stderr().is_terminal(),
        }
    }

    /// The one place that asks where the output is going: the renderer
    /// takes the answer as an input rather than sniffing it itself.
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
    /// Score the changes between a base branch and HEAD (or the index).
    Diff {
        #[command(flatten)]
        diff: DiffArgs,
        #[arg(long, value_enum, default_value = "file")]
        granularity: Granularity,
        #[command(flatten)]
        common: CommonArgs,
    },
    /// Absolute C(tree) of HEAD — the trend-line number.
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
    match cli.cmd {
        None => {
            let common = cli.common;
            let abs = pipeline::abs(&git, &common.abs_options(), common.progress())?;
            let diff = pipeline::diff(&git, &cli.diff.options(&common), common.progress())?;
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
            let report = pipeline::diff(&git, &diff.options(&common), common.progress())?;
            emit(
                common.json,
                &report,
                report::render_diff(&report, common.report_options()),
            )?;
        }
        Some(Cmd::Abs { common }) => {
            let report = pipeline::abs(&git, &common.abs_options(), common.progress())?;
            emit(
                common.json,
                &report,
                report::render_abs(&report, common.report_options()),
            )?;
        }
    }
    Ok(())
}
