use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use crate::diff::matches_pathspecs;
use crate::error::GirError;
use crate::ignore::IgnoreStack;
use crate::index::{GitIndex, IndexEntry};
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::repository::Repository;
use crate::worktree::{
    materialize_blob_to_worktree, prune_empty_parent_dirs, read_commit_tree_map,
};

#[derive(Debug, Clone, Default)]
pub struct RestoreOptions {
    pub source: Option<String>,
    pub staged: bool,
    pub worktree: bool,
    pub pathspecs: Vec<String>,
}

/// Restores specified working-tree and/or index paths from a source tree or the index (`gir restore`).
pub fn restore_paths(repo: &Repository, opts: &RestoreOptions) -> Result<(), GirError> {
    if opts.pathspecs.is_empty() {
        return Err(GirError::InvalidArgument(
            "you must specify path(s) to restore".to_string(),
        ));
    }

    let restore_staged = opts.staged;
    let restore_worktree = if !opts.staged && !opts.worktree {
        true
    } else {
        opts.worktree
    };

    let _ = SafetySnapshotEngine::capture(repo, "restore");

    let mut index = GitIndex::read(repo.git_dir())?;

    // If `--source` is given, or `--staged` is requested without `--source` (which defaults to `HEAD`),
    // load the source tree map.
    let source_spec = if let Some(ref s) = opts.source {
        Some(s.as_str())
    } else if restore_staged {
        Some("HEAD")
    } else {
        None
    };

    if let Some(spec) = source_spec {
        let (_, tree_map) = read_commit_tree_map(repo, spec)?;

        let mut candidate_paths: BTreeSet<String> = tree_map.keys().cloned().collect();
        for entry in index.entries() {
            candidate_paths.insert(entry.path.clone());
        }

        let mut matched_any = false;
        for path in candidate_paths {
            if !matches_pathspecs(&path, &opts.pathspecs) {
                continue;
            }
            matched_any = true;

            match tree_map.get(&path).copied() {
                Some((mode, oid)) => {
                    if restore_worktree {
                        if let Some(worktree) = repo.worktree() {
                            let abs_path = worktree.join(&path);
                            materialize_blob_to_worktree(repo.odb(), worktree, &abs_path, mode, &oid)?;
                            if restore_staged {
                                let meta = fs::symlink_metadata(&abs_path)?;
                                index.add_entry(IndexEntry::from_metadata(&path, oid, 0, &meta));
                            }
                        }
                    } else if restore_staged {
                        index.add_entry(IndexEntry::from_tree_entry(path.clone(), mode, oid, 0));
                    }
                }
                None => {
                    if restore_worktree {
                        if let Some(worktree) = repo.worktree() {
                            let abs_path = worktree.join(&path);
                            if fs::symlink_metadata(&abs_path).is_ok() {
                                let _ = fs::remove_file(&abs_path);
                                prune_empty_parent_dirs(&abs_path, worktree);
                            }
                        }
                    }
                    if restore_staged {
                        index.remove_path(&path);
                    }
                }
            }
        }

        if !matched_any {
            return Err(GirError::InvalidArgument(format!(
                "pathspec '{}' did not match any file(s) known to git",
                opts.pathspecs[0]
            )));
        }

        if restore_staged {
            index.write(repo.git_dir())?;
        }
        return Ok(());
    }

    // Otherwise (`restore_worktree && !restore_staged && opts.source.is_none()`):
    // Restore working tree files directly from `.git/index`!
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let entries_to_restore: Vec<(String, crate::object::tree::FileMode, crate::hash::ObjectId)> =
        index
            .entries()
            .filter(|e| e.stage() == 0 && matches_pathspecs(&e.path, &opts.pathspecs))
            .map(|e| (e.path.clone(), e.file_mode(), e.oid))
            .collect();

    if entries_to_restore.is_empty() {
        return Err(GirError::InvalidArgument(format!(
            "pathspec '{}' did not match any file(s) known to git",
            opts.pathspecs[0]
        )));
    }

    for (path, mode, oid) in entries_to_restore {
        let abs_path = worktree.join(&path);
        materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, mode, &oid)?;
        let meta = fs::symlink_metadata(&abs_path)?;
        index.add_entry(IndexEntry::from_metadata(&path, oid, 0, &meta));
    }

    index.write(repo.git_dir())
}

#[derive(Debug, Clone, Default)]
pub struct CleanOptions {
    pub dry_run: bool,
    pub force: bool,
    pub directories: bool,
    pub include_ignored: bool,
    pub pathspecs: Vec<String>,
}

/// Removes untracked files (and optionally untracked directories `-d` and ignored files `-x`)
/// from the working tree (`gir clean`), capturing a safety snapshot beforehand unless `dry_run`.
pub fn clean_worktree(repo: &Repository, opts: &CleanOptions) -> Result<Vec<String>, GirError> {
    if !opts.dry_run && !opts.force {
        return Err(GirError::InvalidArgument(
            "clean.requireForce defaults to true and neither -i, -n, nor -f given; refusing to clean"
                .to_string(),
        ));
    }

    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    if !opts.dry_run {
        let _ = SafetySnapshotEngine::capture(repo, "clean");
    }

    let index = GitIndex::read(repo.git_dir())?;
    let root_ignore = IgnoreStack::for_repository(repo);
    let mut removed = Vec::new();

    clean_dir_recursive(
        &worktree,
        "",
        &index,
        &root_ignore,
        opts,
        &mut removed,
    )?;

    removed.sort();
    Ok(removed)
}

fn clean_dir_recursive(
    worktree: &Path,
    rel_dir: &str,
    index: &GitIndex,
    ignore_stack: &IgnoreStack,
    opts: &CleanOptions,
    removed: &mut Vec<String>,
) -> Result<(), GirError> {
    let abs_dir = if rel_dir.is_empty() {
        worktree.to_path_buf()
    } else {
        worktree.join(rel_dir)
    };

    let mut entries: Vec<_> = fs::read_dir(&abs_dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
        let fname = entry.file_name();
        let Some(name_str) = fname.to_str() else {
            continue;
        };
        if rel_dir.is_empty() && name_str == ".git" {
            continue;
        }

        let rel_path = if rel_dir.is_empty() {
            name_str.to_string()
        } else {
            format!("{rel_dir}/{name_str}")
        };

        if !matches_pathspecs(&rel_path, &opts.pathspecs)
            && !opts
                .pathspecs
                .iter()
                .any(|s| s.starts_with(&format!("{rel_path}/")))
        {
            continue;
        }

        let abs_path = entry.path();
        let meta = fs::symlink_metadata(&abs_path)?;

        if meta.file_type().is_dir() {
            let has_tracked = index.contains_path_or_dir(&rel_path);
            let is_ignored = ignore_stack.is_ignored(&rel_path, true);

            if !has_tracked {
                if is_ignored && !opts.include_ignored {
                    continue;
                }
                if opts.directories && matches_pathspecs(&rel_path, &opts.pathspecs) {
                    if !opts.dry_run {
                        fs::remove_dir_all(&abs_path)?;
                    }
                    removed.push(format!("{rel_path}/"));
                    continue;
                }
            }

            let child_ignore = ignore_stack.enter_dir(&rel_path, &abs_path);
            clean_dir_recursive(worktree, &rel_path, index, &child_ignore, opts, removed)?;
        } else {
            if index.get(&rel_path, 0).is_some() {
                continue;
            }
            if !opts.include_ignored && ignore_stack.is_ignored(&rel_path, false) {
                continue;
            }
            if !matches_pathspecs(&rel_path, &opts.pathspecs) {
                continue;
            }
            if !opts.dry_run {
                fs::remove_file(&abs_path)?;
            }
            removed.push(rel_path);
        }
    }

    Ok(())
}
