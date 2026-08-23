use anyhow::{Result, bail};
use clap::{Parser, Subcommand, ValueEnum};

use cx_cli::git::Git;
use cx_cli::pipeline::{self, ScoreOptions};
use cx_cli::report;

/// Score git diffs by marginal description length: how much new
/// information a change adds, conditioned on what the codebase already
/// contains.
#[derive(Parser)]
#[command(name = "cx", version)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
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
        Cmd::Score {
            base,
            staged,
            granularity,
            json,
        } => {
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
        Cmd::Tree { json } => {
            let report = pipeline::tree(&git)?;
            if json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                print!("{}", report::render_tree(&report));
            }
        }
    }
    Ok(())
}
