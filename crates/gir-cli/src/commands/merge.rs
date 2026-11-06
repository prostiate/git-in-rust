use std::env;

use clap::Args;
use gir_core::merge::{merge_branch, ConflictStyle, MergeOptions, MergeStatus};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct MergeArgs {
    /// Create a merge commit in all cases, even when the merge could instead be resolved as a fast-forward
    #[arg(long = "no-ff")]
    pub no_ff: bool,

    /// Refuse to merge and exit with a non-zero status unless the current `HEAD` is already up to date or the merge can be resolved as a fast-forward
    #[arg(long = "ff-only", conflicts_with = "no_ff")]
    pub ff_only: bool,

    /// Set the commit message to be used for the merge commit
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// Abort the current conflict resolution process, and try to reconstruct the pre-merge state
    #[arg(long = "abort")]
    pub abort: bool,

    /// After a `gir merge` stops due to conflicts you can conclude the merge by running `gir merge --continue`
    #[arg(long = "continue")]
    pub cont: bool,

    /// Conflict marker style (`merge` or `diff3`)
    #[arg(long = "conflict")]
    pub conflict: Option<String>,

    /// Branch or commit to merge into current `HEAD`
    pub target: Option<String>,
}

pub fn run(args: MergeArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    let conflict_style = match args.conflict.as_deref() {
        Some("diff3") => ConflictStyle::Diff3,
        _ => ConflictStyle::Merge,
    };

    let status = merge_branch(
        &repo,
        &MergeOptions {
            target: args.target,
            no_ff: args.no_ff,
            ff_only: args.ff_only,
            message: args.message,
            abort: args.abort,
            cont: args.cont,
            conflict_style,
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
            println!(
                "Updating {}..{}",
                old_oid.short_hex(),
                new_oid.short_hex()
            );
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
            for desc in &descriptions {
                println!("{desc}");
            }
            Err(GirError::InvalidArgument(
                "Automatic merge failed; fix conflicts and then commit the result.".to_string(),
            ))
        }
        MergeStatus::Aborted => Ok(()),
    }
}
