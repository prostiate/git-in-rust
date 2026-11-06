use std::env;

use clap::Args;
use gir_core::worktree::{clean_worktree, CleanOptions};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct CleanArgs {
    /// Don't actually remove anything, just show what would be done (`-n` / `--dry-run`)
    #[arg(short = 'n', long = "dry-run")]
    pub dry_run: bool,

    /// Force removal of untracked files (`-f` / `--force`)
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Remove untracked directories in addition to untracked files (`-d`)
    #[arg(short = 'd')]
    pub directories: bool,

    /// Don't use the standard ignore rules; remove ignored untracked files too (`-x`)
    #[arg(short = 'x')]
    pub include_ignored: bool,

    /// Optional path prefixes to restrict cleaning
    pub pathspecs: Vec<String>,
}

pub fn run(args: CleanArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let pathspecs = if args.pathspecs.is_empty() {
        Vec::new()
    } else if let Some(wt) = repo.worktree() {
        super::add::normalize_pathspecs(&cwd, wt, &args.pathspecs)
    } else {
        args.pathspecs
    };

    let removed = clean_worktree(
        &repo,
        &CleanOptions {
            dry_run: args.dry_run,
            force: args.force,
            directories: args.directories,
            include_ignored: args.include_ignored,
            pathspecs,
        },
    )?;

    for path in removed {
        if args.dry_run {
            println!("Would remove {path}");
        } else {
            println!("Removing {path}");
        }
    }

    Ok(())
}
