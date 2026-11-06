use std::env;

use clap::Args;
use gir_core::worktree::{restore_paths, RestoreOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct RestoreArgs {
    /// Restore the working tree files with the content from the given tree (`-s` / `--source`)
    #[arg(short = 's', long = "source")]
    pub source: Option<String>,

    /// Restore the content in the index (`-S` / `--staged`)
    #[arg(short = 'S', long = "staged")]
    pub staged: bool,

    /// Restore the working tree (`-W` / `--worktree`; default if neither `-S` nor `-W` is specified)
    #[arg(short = 'W', long = "worktree")]
    pub worktree: bool,

    /// Paths to restore
    pub pathspecs: Vec<String>,
}

pub fn run(args: RestoreArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    if args.pathspecs.is_empty() {
        return Err(GirError::InvalidArgument(
            "you must specify path(s) to restore".to_string(),
        ));
    }

    let pathspecs = if let Some(wt) = repo.worktree() {
        super::add::normalize_pathspecs(&cwd, wt, &args.pathspecs)
    } else {
        args.pathspecs
    };

    restore_paths(
        &repo,
        &RestoreOptions {
            source: args.source,
            staged: args.staged,
            worktree: args.worktree,
            pathspecs,
        },
    )
}
