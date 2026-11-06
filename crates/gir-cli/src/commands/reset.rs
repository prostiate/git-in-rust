use std::env;

use clap::Args;
use gir_core::revision::parse_revision;
use gir_core::worktree::{reset_head, reset_paths, ResetMode};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct ResetArgs {
    /// Does not touch the index file or the working tree at all (but resets the head to `<commit>`)
    #[arg(long = "soft", conflicts_with_all = ["mixed", "hard"])]
    pub soft: bool,

    /// Resets the index but not the working tree (default)
    #[arg(long = "mixed", conflicts_with_all = ["soft", "hard"])]
    pub mixed: bool,

    /// Resets the index and working tree; any changes to tracked files since `<commit>` are discarded
    #[arg(long = "hard", conflicts_with_all = ["soft", "mixed"])]
    pub hard: bool,

    /// Commit and/or paths (`[<commit>] [-- <paths>...]`)
    pub args: Vec<String>,
}

pub fn run(args: ResetArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let mode = if args.hard {
        ResetMode::Hard
    } else if args.soft {
        ResetMode::Soft
    } else {
        ResetMode::Mixed
    };

    if args.args.is_empty() {
        let (oid, subject) = reset_head(&repo, "HEAD", mode)?;
        if mode == ResetMode::Hard {
            println!("HEAD is now at {} {subject}", oid.short_hex());
        }
        return Ok(());
    }

    let first = &args.args[0];
    let first_is_rev = parse_revision(&repo, first).is_ok();

    if args.args.len() == 1 && first_is_rev {
        let (oid, subject) = reset_head(&repo, first, mode)?;
        if mode == ResetMode::Hard {
            println!("HEAD is now at {} {subject}", oid.short_hex());
        }
        return Ok(());
    }

    let (target_spec, raw_paths) = if first_is_rev {
        (first.as_str(), &args.args[1..])
    } else {
        ("HEAD", &args.args[..])
    };

    let pathspecs = if let Some(wt) = repo.worktree() {
        super::add::normalize_pathspecs(&cwd, wt, raw_paths)
    } else {
        raw_paths.to_vec()
    };

    reset_paths(&repo, target_spec, &pathspecs)
}
