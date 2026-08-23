use anyhow::{Result, bail};
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
    #[command(flatten)]
    overview: OverviewArgs,
}

/// Flags for the default (no-subcommand) overview.
#[derive(Args)]
struct OverviewArgs {
    /// Base ref for the diff (default: main/master merge-base).
    #[arg(long)]
    base: Option<String>,
    /// Diff the index instead of HEAD.
    #[arg(long)]
    staged: bool,
    /// Hide the per-file breakdown; show summary lines only.
    #[arg(long)]
    no_files: bool,
    /// Exclude test files (tests/, *_test.*, *.spec.*, …) everywhere.
    #[arg(long)]
    ignore_tests: bool,
    /// Show only the N biggest files/directories in the breakdown.
    #[arg(short = 'n', long, default_value_t = 30)]
    top: usize,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Score the changes between a base branch and HEAD (or the index).
    Diff {
        /// Base ref to diff against (default: main/master merge-base).
        #[arg(long)]
        base: Option<String>,
        /// Diff the index instead of HEAD.
        #[arg(long)]
        staged: bool,
        #[arg(long, value_enum, default_value = "file")]
        granularity: Granularity,
        /// Exclude test files (tests/, *_test.*, *.spec.*, …) from the diff.
        #[arg(long)]
        ignore_tests: bool,
        /// Show only the N biggest files/directories in the breakdown.
        #[arg(short = 'n', long, default_value_t = 30)]
        top: usize,
        #[arg(long)]
        json: bool,
    },
    /// Absolute C(tree) of HEAD — the trend-line number.
    Abs {
        /// Hide per-file contributions.
        #[arg(long)]
        no_files: bool,
        /// Exclude test files (tests/, *_test.*, *.spec.*, …) from C(tree).
        #[arg(long)]
        ignore_tests: bool,
        /// Show only the N biggest files/directories in the breakdown.
        #[arg(short = 'n', long, default_value_t = 30)]
        top: usize,
        #[arg(long)]
        json: bool,
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
            let o = cli.overview;
            let abs = pipeline::abs(
                &git,
                &AbsOptions {
                    with_files: !o.no_files,
                    ignore_tests: o.ignore_tests,
                },
            )?;
            let diff = pipeline::diff(
                &git,
                &DiffOptions {
                    base: o.base,
                    staged: o.staged,
                    ignore_tests: o.ignore_tests,
                },
            )?;
            let rendered = if o.no_files {
                format!(
                    "{}\n{}",
                    report::render_abs(&abs, o.top),
                    report::render_diff(&diff, o.top)
                )
            } else {
                report::render_overview(&abs, &diff, o.top)
            };
            emit(
                o.json,
                &serde_json::json!({ "abs": abs, "diff": diff }),
                rendered,
            )?;
        }
        Some(Cmd::Diff {
            base,
            staged,
            granularity,
            ignore_tests,
            top,
            json,
        }) => {
            if matches!(granularity, Granularity::Hunk) {
                bail!("hunk granularity is not implemented yet (plan phase 3)");
            }
            let report = pipeline::diff(
                &git,
                &DiffOptions {
                    base,
                    staged,
                    ignore_tests,
                },
            )?;
            emit(json, &report, report::render_diff(&report, top))?;
        }
        Some(Cmd::Abs {
            no_files,
            ignore_tests,
            top,
            json,
        }) => {
            let report = pipeline::abs(
                &git,
                &AbsOptions {
                    with_files: !no_files,
                    ignore_tests,
                },
            )?;
            emit(json, &report, report::render_abs(&report, top))?;
        }
    }
    Ok(())
}
