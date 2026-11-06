use std::env;

use clap::Args;
use gir_core::rebase::{cherry_pick, CherryPickOptions, CherryPickStatus};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct CherryPickArgs {
    /// Usually the command automatically creates a sequence of commits; this flag applies the changes to your working tree and index without making any commit (`-n` / `--no-commit`)
    #[arg(short = 'n', long = "no-commit")]
    pub no_commit: bool,

    /// Continue the operation in progress using the information in `.git/sequencer`
    #[arg(long = "continue")]
    pub cont: bool,

    /// Skip the current commit and continue with the rest of the sequence
    #[arg(long = "skip")]
    pub skip: bool,

    /// Cancel the operation and return to the pre-sequence state
    #[arg(long = "abort")]
    pub abort: bool,

    /// Commits to cherry-pick
    pub commits: Vec<String>,
}

pub fn run(args: CherryPickArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let status = cherry_pick(
        &repo,
        &CherryPickOptions {
            commits: args.commits,
            no_commit: args.no_commit,
            cont: args.cont,
            abort: args.abort,
            skip: args.skip,
        },
    )?;

    match status {
        CherryPickStatus::Applied { new_commits } => {
            for oid in new_commits {
                println!("[cherry-pick {}]", oid.short_hex());
            }
            Ok(())
        }
        CherryPickStatus::Conflicted {
            commit_oid,
            descriptions,
            ..
        } => {
            for desc in &descriptions {
                println!("{desc}");
            }
            Err(GirError::InvalidArgument(format!(
                "could not apply {}",
                commit_oid.short_hex()
            )))
        }
        CherryPickStatus::Aborted => Ok(()),
    }
}
