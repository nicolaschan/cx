use anyhow::{Result, bail};
use clap::builder::BoolishValueParser;
use clap::{Args, Parser, Subcommand, ValueEnum};

use cx_cli::git::Git;
use cx_cli::pipeline::{self, AbsOptions, DiffOptions};
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
    /// Hide the per-file breakdown; show summary lines only.
    #[arg(long)]
    no_files: bool,
    #[command(flatten)]
    diff: DiffArgs,
    #[command(flatten)]
    common: CommonArgs,
}

/// Flags every view accepts, declared once so a default pinned through
/// the environment means the same thing in all of them.
#[derive(Args)]
struct CommonArgs {
    /// Exclude test files everywhere: no reference, no scoring, listed
    /// as skipped. Detected by naming convention, not by language.
    ///
    /// Accepts a value so an environment default can be turned back off
    /// for one run: `--ignore-tests=false`.
    #[arg(
        long,
        env = "CX_IGNORE_TESTS",
        num_args = 0..=1,
        default_value_t = false,
        default_missing_value = "true",
        value_parser = BoolishValueParser::new(),
    )]
    ignore_tests: bool,
    /// Show only the N biggest files/directories in the breakdown.
    #[arg(short = 'n', long, env = "CX_TOP", default_value_t = 30)]
    top: usize,
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
    fn abs_options(&self, no_files: bool) -> AbsOptions {
        AbsOptions {
            with_files: !no_files,
            ignore_tests: self.ignore_tests,
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
        /// Hide per-file contributions.
        #[arg(long)]
        no_files: bool,
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
            let abs = pipeline::abs(&git, &common.abs_options(cli.no_files))?;
            let diff = pipeline::diff(&git, &cli.diff.options(&common))?;
            let rendered = if cli.no_files {
                format!(
                    "{}\n{}",
                    report::render_abs(&abs, common.top),
                    report::render_diff(&diff, common.top)
                )
            } else {
                report::render_overview(&abs, &diff, common.top)
            };
            emit(
                common.json,
                &serde_json::json!({ "abs": abs, "diff": diff }),
                rendered,
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
            let report = pipeline::diff(&git, &diff.options(&common))?;
            emit(
                common.json,
                &report,
                report::render_diff(&report, common.top),
            )?;
        }
        Some(Cmd::Abs { no_files, common }) => {
            let report = pipeline::abs(&git, &common.abs_options(no_files))?;
            emit(
                common.json,
                &report,
                report::render_abs(&report, common.top),
            )?;
        }
    }
    Ok(())
}
