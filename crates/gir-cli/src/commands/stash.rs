use std::env;

use clap::{Args, Subcommand};
use gir_core::diff::{diff_tree_to_tree, summarize_patches};
use gir_core::graph::peel_to_commit_object;
use gir_core::stash::{
    resolve_stash_spec, stash_apply, stash_clear, stash_drop, stash_list, stash_pop, stash_push,
};
use gir_core::{GirError, Repository};

#[derive(Debug, Args)]
pub struct StashArgs {
    /// Save your local modifications to a new stash entry (`-m` shorthand at top level)
    #[arg(short = 'm', long = "message")]
    pub message: Option<String>,

    /// Include untracked files when stashing (`-u` shorthand at top level)
    #[arg(short = 'u', long = "include-untracked")]
    pub include_untracked: bool,

    #[command(subcommand)]
    pub command: Option<StashSubcommand>,
}

#[derive(Debug, Subcommand)]
pub enum StashSubcommand {
    /// Save your local modifications to a new stash entry and roll them back to HEAD
    Push {
        #[arg(short = 'm', long = "message")]
        message: Option<String>,

        #[arg(short = 'u', long = "include-untracked")]
        include_untracked: bool,
    },

    /// List the stash entries that you currently have
    List,

    /// Inspect the changes recorded in a stash entry as a diff against its base commit
    Show {
        #[arg(short = 'p', long = "patch")]
        patch: bool,

        stash: Option<String>,
    },

    /// Like `pop`, but do not remove the state from the stash list
    Apply {
        #[arg(long = "index")]
        index: bool,

        stash: Option<String>,
    },

    /// Remove a single stashed state from the stash list and apply it on top of the current working tree
    Pop {
        #[arg(long = "index")]
        index: bool,

        stash: Option<String>,
    },

    /// Remove a single stash entry from the list of stash entries
    Drop {
        stash: Option<String>,
    },

    /// Remove all the stash entries
    Clear,
}

pub fn run(args: StashArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;

    match args.command {
        None => {
            let (_, summary) = stash_push(
                &repo,
                args.message.as_deref(),
                args.include_untracked,
            )?;
            println!("Saved working directory and index state {summary}");
            Ok(())
        }
        Some(StashSubcommand::Push {
            message,
            include_untracked,
        }) => {
            let (_, summary) = stash_push(
                &repo,
                message.as_deref(),
                include_untracked,
            )?;
            println!("Saved working directory and index state {summary}");
            Ok(())
        }
        Some(StashSubcommand::List) => {
            let entries = stash_list(&repo)?;
            for e in entries {
                println!("stash@{{{}}}: {}", e.index, e.message);
            }
            Ok(())
        }
        Some(StashSubcommand::Show { patch, stash }) => {
            let (_, stash_oid) = resolve_stash_spec(&repo, stash.as_deref())?;
            let (_, w_commit) = peel_to_commit_object(repo.odb(), &stash_oid)?;
            let base_tree = if let Some(b_oid) = w_commit.parents.first() {
                let (_, b_commit) = peel_to_commit_object(repo.odb(), b_oid)?;
                Some(b_commit.tree)
            } else {
                None
            };

            let patches =
                diff_tree_to_tree(repo.odb(), base_tree.as_ref(), &w_commit.tree, &[])?;
            if patch {
                for p in &patches {
                    print!("{}", p.format_unified());
                }
            } else {
                let summary = summarize_patches(&patches);
                print!("{}", summary.format_stat_block());
            }
            Ok(())
        }
        Some(StashSubcommand::Apply { index, stash }) => {
            stash_apply(&repo, stash.as_deref(), index)?;
            Ok(())
        }
        Some(StashSubcommand::Pop { index, stash }) => {
            let (idx, oid) = stash_pop(&repo, stash.as_deref(), index)?;
            println!("Dropped refs/stash@{{{idx}}} ({oid})");
            Ok(())
        }
        Some(StashSubcommand::Drop { stash }) => {
            let (idx, oid) = stash_drop(&repo, stash.as_deref())?;
            println!("Dropped refs/stash@{{{idx}}} ({oid})");
            Ok(())
        }
        Some(StashSubcommand::Clear) => stash_clear(&repo),
    }
}
