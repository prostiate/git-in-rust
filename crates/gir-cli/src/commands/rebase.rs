use std::env;

use clap::Args;
use gir_core::rebase::{run_rebase, RebaseOptions, RebaseStatus};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct RebaseArgs {
    /// Make a list of the commits which are about to be rebased and let the user edit that list before rebasing (`-i` / `--interactive`)
    #[arg(short = 'i', long = "interactive")]
    pub interactive: bool,

    /// Starting point at which to create the new commits
    #[arg(long = "onto")]
    pub onto: Option<String>,

    /// Restart the rebasing process after having resolved a merge conflict
    #[arg(long = "continue")]
    pub cont: bool,

    /// Restart the rebasing process by skipping the current patch
    #[arg(long = "skip")]
    pub skip: bool,

    /// Abort the rebase operation and reset HEAD to the original branch
    #[arg(long = "abort")]
    pub abort: bool,

    /// Upstream branch to compare against
    pub upstream: Option<String>,
}

pub fn run(args: RebaseArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let status = run_rebase(
        &repo,
        &RebaseOptions {
            upstream: args.upstream,
            onto: args.onto,
            interactive: args.interactive,
            cont: args.cont,
            abort: args.abort,
            skip: args.skip,
        },
    )?;

    match status {
        RebaseStatus::UpToDate => {
            println!("Current branch is up to date.");
            Ok(())
        }
        RebaseStatus::Completed { branch_name, .. } => {
            eprintln!("Successfully rebased and updated {branch_name}.");
            Ok(())
        }
        RebaseStatus::Paused {
            commit_oid,
            reason,
            descriptions,
            ..
        } => {
            if reason == "edit" {
                eprintln!(
                    "Stopped at {}... You can amend the commit now, and then run 'gir rebase --continue'",
                    commit_oid.short_hex()
                );
                Ok(())
            } else {
                for desc in &descriptions {
                    println!("{desc}");
                }
                Err(GirError::InvalidArgument(format!(
                    "Could not apply {}... Resolve all conflicts manually, mark them as resolved with \"gir add/rm <conflicted_files>\", then run \"gir rebase --continue\".",
                    commit_oid.short_hex()
                )))
            }
        }
        RebaseStatus::Aborted => Ok(()),
    }
}
