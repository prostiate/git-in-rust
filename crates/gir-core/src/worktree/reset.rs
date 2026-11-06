use crate::diff::matches_pathspecs;
use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::index::{write_index_to_tree, GitIndex, IndexEntry};
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::worktree::{checkout_tree, fallback_sig, read_commit_tree_map};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetMode {
    Soft,
    Mixed,
    Hard,
}

/// Resets `HEAD` (and the active branch) to `target_spec`, updating `.git/index` (`Mixed` / `Hard`)
/// and the working tree (`Hard`).
pub fn reset_head(
    repo: &Repository,
    target_spec: &str,
    mode: ResetMode,
) -> Result<(ObjectId, String), GirError> {
    let raw_oid = parse_revision(repo, target_spec)?;
    let (commit_oid, commit) = peel_to_commit_object(repo.odb(), &raw_oid)?;
    let subject = commit.subject().to_string();

    match mode {
        ResetMode::Soft => {}
        ResetMode::Mixed => {
            let _ = SafetySnapshotEngine::capture(repo, &format!("reset --mixed {target_spec}"));
            let mut index = GitIndex::read(repo.git_dir())?;
            index.populate_from_tree(repo.odb(), &commit.tree)?;
            let _ = write_index_to_tree(&mut index, repo.odb());
            index.write(repo.git_dir())?;
        }
        ResetMode::Hard => {
            checkout_tree(
                repo,
                &commit.tree,
                true,
                &format!("reset --hard {target_spec}"),
            )?;
        }
    }

    let sig = fallback_sig(repo);
    let reflog_msg = format!("reset: moving to {target_spec}");
    repo.refs()
        .update_ref("HEAD", commit_oid, None, false, &sig, &reflog_msg)?;

    Ok((commit_oid, subject))
}

/// Resets specific `pathspecs` in `.git/index` to their state in `target_spec` (default `"HEAD"`).
pub fn reset_paths(
    repo: &Repository,
    target_spec: &str,
    pathspecs: &[String],
) -> Result<(), GirError> {
    let tree_map = read_commit_tree_map(repo, target_spec)
        .map(|(_, m)| m)
        .unwrap_or_default();

    let mut index = GitIndex::read(repo.git_dir())?;

    // Remove matching entries currently in index
    let current_paths: Vec<String> = index
        .entries()
        .map(|e| e.path.clone())
        .filter(|p| matches_pathspecs(p, pathspecs))
        .collect();
    for p in current_paths {
        index.remove_path(&p);
    }

    // Re-insert matching entries from `tree_map`
    for (path, &(mode, oid)) in &tree_map {
        if matches_pathspecs(path, pathspecs) {
            index.add_entry(IndexEntry::from_tree_entry(path.clone(), mode, oid, 0));
        }
    }

    index.write(repo.git_dir())
}
