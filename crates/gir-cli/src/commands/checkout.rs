use std::env;

use clap::Args;
use gir_core::index::GitIndex;
use gir_core::revision::parse_revision;
use gir_core::worktree::{
    restore_paths, switch_or_checkout, RestoreOptions, SwitchOptions,
};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct CheckoutArgs {
    /// Create a new branch named `<new-branch>` and start it at `<start-point>`
    #[arg(short = 'b')]
    pub create_branch: Option<String>,

    /// Create or reset a branch named `<new-branch>` and start it at `<start-point>`
    #[arg(short = 'B')]
    pub force_create_branch: Option<String>,

    /// Check out a commit in detached HEAD state
    #[arg(short = 'd', long = "detach")]
    pub detach: bool,

    /// Proceed even if the index or the working tree differs from HEAD
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Branch/commit and/or paths to check out (`[<branch-or-commit>] [-- <paths>...]`)
    pub args: Vec<String>,
}

pub fn run(args: CheckoutArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let (create_branch, force_create) = if let Some(b) = args.force_create_branch {
        (Some(b), true)
    } else if let Some(b) = args.create_branch {
        (Some(b), false)
    } else {
        (None, false)
    };

    // If `-b` / `-B` is specified, always perform a branch creation + switch
    if create_branch.is_some() {
        let target = args.args.first().cloned();
        let outcome = switch_or_checkout(
            &repo,
            &SwitchOptions {
                target,
                create_branch,
                force_create,
                detach: args.detach,
                force: args.force,
            },
        )?;
        if let Some(ref b) = outcome.new_branch {
            eprintln!("Switched to a new branch '{b}'");
        }
        return Ok(());
    }

    if args.args.is_empty() {
        return Err(GirError::InvalidArgument(
            "you must specify a branch, commit, or path to checkout".to_string(),
        ));
    }

    // Disambiguate `gir checkout <branch>` vs `gir checkout [<tree-ish>] -- <paths>...`
    let first = &args.args[0];
    let is_first_rev = first == "-"
        || repo.refs().resolve_dwim(first).is_ok()
        || parse_revision(&repo, first).is_ok();

    if args.args.len() == 1 && is_first_rev {
        let outcome = switch_or_checkout(
            &repo,
            &SwitchOptions {
                target: Some(first.clone()),
                create_branch: None,
                force_create: false,
                detach: args.detach,
                force: args.force,
            },
        )?;
        if let Some(ref b) = outcome.new_branch {
            if outcome.previous_desc == *b {
                eprintln!("Already on '{b}'");
            } else {
                eprintln!("Switched to branch '{b}'");
            }
        } else {
            eprintln!("HEAD is now at {}", outcome.target_commit.short_hex());
        }
        return Ok(());
    }

    // Path checkout mode (`gir checkout [<tree-ish>] <paths>...`)
    let index = GitIndex::read(repo.git_dir())?;
    let (source, raw_paths) = if is_first_rev && !index.contains_path_or_dir(first) {
        (Some(first.clone()), &args.args[1..])
    } else {
        (None, &args.args[..])
    };

    let pathspecs = if let Some(wt) = repo.worktree() {
        super::add::normalize_pathspecs(&cwd, wt, raw_paths)
    } else {
        raw_paths.to_vec()
    };

    restore_paths(
        &repo,
        &RestoreOptions {
            source: source.clone(),
            staged: source.is_some(),
            worktree: true,
            pathspecs,
        },
    )
}
