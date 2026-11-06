use std::env;

use clap::Args;
use gir_core::merge::{merge_branch, MergeOptions, MergeStatus};
use gir_core::rebase::{run_rebase, RebaseOptions, RebaseStatus};
use gir_core::refs::RefTarget;
use gir_core::{GirError, Repository};
use gir_transport::fetch_from_remote;

#[derive(Debug, Args)]
pub struct PullArgs {
    /// Rebase the current branch on top of the upstream branch after fetching (`-r` / `--rebase`)
    #[arg(short = 'r', long = "rebase")]
    pub rebase: bool,

    /// Refuse to merge and exit with a non-zero status unless the current `HEAD` is already up to date or the merge can be resolved as a fast-forward
    #[arg(long = "ff-only", conflicts_with = "no_ff")]
    pub ff_only: bool,

    /// Create a merge commit in all cases, even when the merge could instead be resolved as a fast-forward
    #[arg(long = "no-ff")]
    pub no_ff: bool,

    /// Remote name or URL (defaults to current branch's configured `branch.<name>.remote` or `"origin"`)
    pub remote: Option<String>,

    /// Remote branch to integrate (defaults to current branch's configured `branch.<name>.merge` or current branch name)
    pub branch: Option<String>,
}

pub fn run(args: PullArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let current_branch = match repo.refs().read_raw("HEAD")? {
        Some(RefTarget::Symbolic(sym)) => sym
            .strip_prefix("refs/heads/")
            .unwrap_or(&sym)
            .to_string(),
        _ => {
            return Err(GirError::InvalidArgument(
                "You are not currently on a branch.".to_string(),
            ));
        }
    };

    let cfg = repo.config()?;
    let remote = args
        .remote
        .clone()
        .or_else(|| {
            cfg.get(&format!("branch.{current_branch}.remote"))
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "origin".to_string());

    let remote_branch_short = args
        .branch
        .clone()
        .or_else(|| {
            cfg.get(&format!("branch.{current_branch}.merge"))
                .map(|m| m.strip_prefix("refs/heads/").unwrap_or(m).to_string())
        })
        .unwrap_or_else(|| current_branch.clone());

    let refspecs = vec![remote_branch_short.clone()];
    let fetch_outcome = fetch_from_remote(&repo, &remote, &refspecs)?;

    let merge_target = fetch_outcome
        .updated_refs
        .first()
        .and_then(|u| {
            u.local_tracking_ref
                .clone()
                .or_else(|| Some(u.new_oid.to_hex()))
        })
        .unwrap_or_else(|| format!("refs/remotes/{remote}/{remote_branch_short}"));

    if args.rebase {
        let rb_status = run_rebase(
            &repo,
            &RebaseOptions {
                upstream: Some(merge_target),
                ..Default::default()
            },
        )?;
        match rb_status {
            RebaseStatus::UpToDate => println!("Already up to date."),
            RebaseStatus::Completed { branch_name, .. } => {
                eprintln!("Successfully rebased and updated {branch_name}.");
            }
            RebaseStatus::Paused {
                commit_oid,
                descriptions,
                ..
            } => {
                for d in &descriptions {
                    println!("{d}");
                }
                return Err(GirError::InvalidArgument(format!(
                    "Could not apply {} during pull --rebase",
                    commit_oid.short_hex()
                )));
            }
            RebaseStatus::Aborted => {}
        }
        return Ok(());
    }

    let status = merge_branch(
        &repo,
        &MergeOptions {
            target: Some(merge_target),
            no_ff: args.no_ff,
            ff_only: args.ff_only,
            message: Some(format!(
                "Merge branch '{remote_branch_short}' of {}",
                fetch_outcome.remote_url
            )),
            ..Default::default()
        },
    )?;

    match status {
        MergeStatus::UpToDate => {
            println!("Already up to date.");
            Ok(())
        }
        MergeStatus::FastForward {
            old_oid,
            new_oid,
            diff_summary,
        } => {
            println!("Updating {}..{}", old_oid.short_hex(), new_oid.short_hex());
            println!("Fast-forward");
            if !diff_summary.files.is_empty() {
                print!("{}", diff_summary.format_stat_block());
            }
            Ok(())
        }
        MergeStatus::Merged { diff_summary, .. } => {
            println!("Merge made by the 'ort' strategy.");
            if !diff_summary.files.is_empty() {
                print!("{}", diff_summary.format_stat_block());
            }
            Ok(())
        }
        MergeStatus::Conflicted { descriptions, .. } => {
            for d in &descriptions {
                println!("{d}");
            }
            Err(GirError::InvalidArgument(
                "Automatic merge failed; fix conflicts and then commit the result.".to_string(),
            ))
        }
        MergeStatus::Aborted => Ok(()),
    }
}
