use anyhow::{Result, bail};
use clap::{Args, Parser, Subcommand, ValueEnum};

use cx_cli::git::Git;
use cx_cli::pipeline::{self, ScoreOptions, TreeOptions};
use cx_cli::report;

/// Score git diffs by marginal description length: how much new
/// information a change adds, conditioned on what the codebase already
/// contains. With no subcommand, shows the tree score (with per-file
/// contributions) and the diff score.
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
    /// Base ref for the diff section (default: main/master merge-base).
    #[arg(long)]
    base: Option<String>,
    /// Score the index instead of HEAD in the diff section.
    #[arg(long)]
    staged: bool,
    /// Hide per-file contributions in the tree section.
    #[arg(long)]
    no_files: bool,
    #[arg(long)]
    json: bool,
}

#[derive(Subcommand)]
enum Cmd {
    /// Score the changes between a base branch and HEAD (or the index).
    Score {
        /// Base ref to diff against (default: main/master merge-base).
        #[arg(long)]
        base: Option<String>,
        /// Score the index instead of HEAD.
        #[arg(long)]
        staged: bool,
        #[arg(long, value_enum, default_value = "file")]
        granularity: Granularity,
        #[arg(long)]
        json: bool,
    },
    /// Absolute C(tree) of HEAD — the trend-line number.
    Tree {
        /// Hide per-file contributions.
        #[arg(long)]
        no_files: bool,
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
            let tree = pipeline::tree(
                &git,
                &TreeOptions {
                    with_files: !o.no_files,
                },
            )?;
            let score = pipeline::score(
                &git,
                &ScoreOptions {
                    base: o.base,
                    staged: o.staged,
                },
            )?;
            if o.json {
                let combined = serde_json::json!({ "tree": tree, "score": score });
                println!("{}", serde_json::to_string_pretty(&combined)?);
            } else {
                print!("{}", report::render_tree(&tree));
                println!();
                print!("{}", report::render_score(&score));
            }
        }
        Some(Cmd::Score {
            base,
            staged,
            granularity,
            json,
        }) => {
            if matches!(granularity, Granularity::Hunk) {
                bail!("hunk granularity is not implemented yet (plan phase 3)");
            }
            let report = pipeline::score(&git, &ScoreOptions { base, staged })?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_score(&report));
            }
        }
        Some(Cmd::Tree { no_files, json }) => {
            let report = pipeline::tree(
                &git,
                &TreeOptions {
                    with_files: !no_files,
                },
            )?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_tree(&report));
            }
        }
    }
    Ok(())
}
