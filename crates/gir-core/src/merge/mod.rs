pub mod file_merge;
pub mod lca;
pub mod tree_merge;

use std::fs;
use std::io::Write;

use crate::commit_ops::{create_commit, diff_trees, CommitOptions, TreeDiffSummary};
use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::storage::atomic::LockFile;
use crate::worktree::{checkout_tree, fallback_sig};

pub use file_merge::{merge_blobs_3way, ConflictStyle, MergeFileResult, MergeLabels};
pub use lca::find_merge_bases;
pub use tree_merge::{
    apply_tree_merge_to_worktree, merge_trees_3way, MergedPathState, TreeMergeOutcome,
};

#[derive(Debug, Clone, Default)]
pub struct MergeOptions {
    pub target: Option<String>,
    pub no_ff: bool,
    pub ff_only: bool,
    pub message: Option<String>,
    pub abort: bool,
    pub cont: bool,
    pub conflict_style: ConflictStyle,
}

#[derive(Debug, Clone)]
pub enum MergeStatus {
    UpToDate,
    FastForward {
        old_oid: ObjectId,
        new_oid: ObjectId,
        diff_summary: TreeDiffSummary,
    },
    Merged {
        commit_oid: ObjectId,
        diff_summary: TreeDiffSummary,
    },
    Conflicted {
        conflicts: Vec<String>,
        descriptions: Vec<String>,
    },
    Aborted,
}

/// Executes `gir merge` (supporting fast-forward, `--no-ff`, `--ff-only`, 3-way merge, `--abort`, and `--continue`).
pub fn merge_branch(repo: &Repository, opts: &MergeOptions) -> Result<MergeStatus, GirError> {
    if opts.abort {
        return merge_abort(repo);
    }
    if opts.cont {
        return merge_continue(repo, opts.message.as_deref());
    }

    let merge_head_path = repo.git_dir().join("MERGE_HEAD");
    if merge_head_path.is_file() {
        return Err(GirError::InvalidArgument(
            "You have not concluded your merge (MERGE_HEAD exists).\nPlease, commit your changes before you merge."
                .to_string(),
        ));
    }

    let Some(ref target_spec) = opts.target else {
        return Err(GirError::InvalidArgument(
            "No commit specified and merge.defaultToUpstream not set.".to_string(),
        ));
    };

    let head_raw = repo.refs().resolve_to_oid("HEAD").map_err(|_| {
        GirError::InvalidArgument("HEAD does not point to a valid commit".to_string())
    })?;
    let (ours_oid, ours_commit) = peel_to_commit_object(repo.odb(), &head_raw)?;

    let target_raw = parse_revision(repo, target_spec)?;
    let (theirs_oid, theirs_commit) = peel_to_commit_object(repo.odb(), &target_raw)?;

    let bases = find_merge_bases(repo.odb(), &ours_oid, &theirs_oid)?;

    // 1. Check if `theirs` is already an ancestor of `ours` (`HEAD`)
    if bases.contains(&theirs_oid) {
        return Ok(MergeStatus::UpToDate);
    }

    // 2. Check if `ours` (`HEAD`) is an ancestor of `theirs` (Fast-forward eligible)
    if bases.contains(&ours_oid) && !opts.no_ff {
        checkout_tree(
            repo,
            &theirs_commit.tree,
            false,
            &format!("merge {target_spec}"),
        )?;

        write_state_file(repo.git_dir(), "ORIG_HEAD", &format!("{ours_oid}\n"))?;
        let sig = fallback_sig(repo);
        let reflog_msg = format!("merge {target_spec}: Fast-forward");
        repo.refs().update_ref(
            "HEAD",
            theirs_oid,
            Some(ours_oid),
            false,
            &sig,
            &reflog_msg,
        )?;

        let diff_summary =
            diff_trees(repo.odb(), Some(&ours_commit.tree), &theirs_commit.tree)?;
        return Ok(MergeStatus::FastForward {
            old_oid: ours_oid,
            new_oid: theirs_oid,
            diff_summary,
        });
    }

    if opts.ff_only {
        return Err(GirError::InvalidArgument(
            "Not possible to fast-forward, aborting.".to_string(),
        ));
    }

    // 3. True 3-Way Merge
    let _ = SafetySnapshotEngine::capture(repo, &format!("merge {target_spec}"));

    let base_tree = if let Some(base_oid) = bases.first() {
        let (_, bc) = peel_to_commit_object(repo.odb(), base_oid)?;
        Some(bc.tree)
    } else {
        None
    };

    let labels = MergeLabels {
        ours: "HEAD",
        base: "merged common ancestors",
        theirs: target_spec,
    };

    let tree_outcome = merge_trees_3way(
        repo.odb(),
        base_tree.as_ref(),
        &ours_commit.tree,
        &theirs_commit.tree,
        labels,
        opts.conflict_style,
    )?;

    apply_tree_merge_to_worktree(repo, &ours_commit.tree, &tree_outcome)?;

    write_state_file(repo.git_dir(), "ORIG_HEAD", &format!("{ours_oid}\n"))?;
    let merge_msg = opts
        .message
        .clone()
        .unwrap_or_else(|| format!("Merge branch '{target_spec}'"));

    write_state_file(repo.git_dir(), "MERGE_HEAD", &format!("{theirs_oid}\n"))?;
    write_state_file(repo.git_dir(), "MERGE_MSG", &format!("{merge_msg}\n"))?;
    let mode_contents = if opts.no_ff { "no-ff" } else { "" };
    write_state_file(repo.git_dir(), "MERGE_MODE", mode_contents)?;

    if tree_outcome.has_conflicts() {
        let mut descriptions = Vec::new();
        for state in tree_outcome.paths.values() {
            if let MergedPathState::Conflict { description, .. } = state {
                descriptions.push(description.clone());
            }
        }
        return Ok(MergeStatus::Conflicted {
            conflicts: tree_outcome.conflicts,
            descriptions,
        });
    }

    // Clean 3-way merge: create the 2-parent merge commit immediately
    let commit_outcome = create_commit(
        repo,
        &CommitOptions {
            message: merge_msg,
            allow_empty: true,
            ..Default::default()
        },
    )?;
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MODE"));

    Ok(MergeStatus::Merged {
        commit_oid: commit_outcome.commit_oid,
        diff_summary: commit_outcome.diff_summary,
    })
}

fn merge_abort(repo: &Repository) -> Result<MergeStatus, GirError> {
    let merge_head_path = repo.git_dir().join("MERGE_HEAD");
    if !merge_head_path.is_file() {
        return Err(GirError::InvalidArgument(
            "There is no merge to abort (MERGE_HEAD missing).".to_string(),
        ));
    }

    let orig_oid = read_oid_file(&repo.git_dir().join("ORIG_HEAD"))
        .or_else(|_| repo.refs().resolve_to_oid("HEAD"))?;
    let (commit_oid, orig_commit) = peel_to_commit_object(repo.odb(), &orig_oid)?;

    checkout_tree(repo, &orig_commit.tree, true, "merge --abort")?;
    let sig = fallback_sig(repo);
    repo.refs()
        .update_ref("HEAD", commit_oid, None, false, &sig, "merge --abort: updating HEAD")?;

    let _ = fs::remove_file(merge_head_path);
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MODE"));

    Ok(MergeStatus::Aborted)
}

fn merge_continue(repo: &Repository, msg_override: Option<&str>) -> Result<MergeStatus, GirError> {
    let merge_head_path = repo.git_dir().join("MERGE_HEAD");
    if !merge_head_path.is_file() {
        return Err(GirError::InvalidArgument(
            "There is no merge in progress (MERGE_HEAD missing).".to_string(),
        ));
    }

    let message = match msg_override {
        Some(m) => m.to_string(),
        None => fs::read_to_string(repo.git_dir().join("MERGE_MSG"))
            .unwrap_or_else(|_| "Merge branch".to_string()),
    };

    let outcome = create_commit(
        repo,
        &CommitOptions {
            message,
            allow_empty: true,
            ..Default::default()
        },
    )?;
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MODE"));

    Ok(MergeStatus::Merged {
        commit_oid: outcome.commit_oid,
        diff_summary: outcome.diff_summary,
    })
}

pub(crate) fn write_state_file(
    git_dir: &std::path::Path,
    name: &str,
    content: &str,
) -> Result<(), GirError> {
    let path = git_dir.join(name);
    let mut lock = LockFile::acquire(&path)?;
    lock.write_all(content.as_bytes())?;
    lock.commit()
}

pub(crate) fn read_oid_file(path: &std::path::Path) -> Result<ObjectId, GirError> {
    let content = fs::read_to_string(path)?;
    ObjectId::from_hex(content.trim())
}
