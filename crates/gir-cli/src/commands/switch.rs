use std::env;

use clap::Args;
use gir_core::worktree::{switch_or_checkout, SwitchOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct SwitchArgs {
    /// Create a new branch named `<new-branch>` starting at `<start-point>` and switch to it
    #[arg(short = 'c', long = "create")]
    pub create: Option<String>,

    /// Like `-c`, but resets `<new-branch>` if it already exists
    #[arg(short = 'C', long = "force-create")]
    pub force_create: Option<String>,

    /// Switch to a commit for inspection and discardable experiments (detached HEAD)
    #[arg(short = 'd', long = "detach")]
    pub detach: bool,

    /// Proceed even if the index or the working tree differs from HEAD (`-f` / `--force`)
    #[arg(short = 'f', long = "force", visible_alias = "discard-changes")]
    pub force: bool,

    /// Branch to switch to (or `<start-point>` when using `-c` / `-C`)
    pub target: Option<String>,
}

pub fn run(args: SwitchArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let (create_branch, force_create) = if let Some(b) = args.force_create {
        (Some(b), true)
    } else if let Some(b) = args.create {
        (Some(b), false)
    } else {
        (None, false)
    };

    let outcome = switch_or_checkout(
        &repo,
        &SwitchOptions {
            target: args.target,
            create_branch,
            force_create,
            detach: args.detach,
            force: args.force,
        },
    )?;

    if let Some(ref b) = outcome.new_branch {
        if outcome.was_created {
            eprintln!("Switched to a new branch '{b}'");
        } else if outcome.previous_desc == *b {
            eprintln!("Already on '{b}'");
        } else {
            eprintln!("Switched to branch '{b}'");
        }
    } else if outcome.is_detached {
        eprintln!(
            "HEAD is now at {}",
            outcome.target_commit.short_hex()
        );
    }

    Ok(())
}
