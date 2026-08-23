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
                },
            )?;
            let diff = pipeline::diff(
                &git,
                &DiffOptions {
                    base: o.base,
                    staged: o.staged,
                },
            )?;
            if o.json {
                let combined = serde_json::json!({ "abs": abs, "diff": diff });
                println!("{}", serde_json::to_string_pretty(&combined)?);
            } else if o.no_files {
                print!("{}", report::render_abs(&abs, o.top));
                println!();
                print!("{}", report::render_diff(&diff, o.top));
            } else {
                print!("{}", report::render_overview(&abs, &diff, o.top));
            }
        }
        Some(Cmd::Diff {
            base,
            staged,
            granularity,
            top,
            json,
        }) => {
            if matches!(granularity, Granularity::Hunk) {
                bail!("hunk granularity is not implemented yet (plan phase 3)");
            }
            let report = pipeline::diff(&git, &DiffOptions { base, staged })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_diff(&report, top));
            }
        }
        Some(Cmd::Abs {
            no_files,
            top,
            json,
        }) => {
            let report = pipeline::abs(
                &git,
                &AbsOptions {
                    with_files: !no_files,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_abs(&report, top));
            }
        }
    }
    Ok(())
}
