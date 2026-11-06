use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::path::Path;

use crate::diff::{
    apply_hunks_to_blob, build_hunks_from_bytes, mode_from_metadata, read_worktree_entry,
};
use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::ignore::IgnoreStack;
use crate::index::{flatten_tree, write_index_to_tree, GitIndex, IndexEntry};
use crate::object::commit::Commit;
use crate::object::ObjectKind;
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::reflog::{append_reflog, read_reflog, resolve_reflog_at};
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::storage::atomic::LockFile;
use crate::worktree::{
    checkout_tree, fallback_sig, materialize_blob_to_worktree, prune_empty_parent_dirs,
};

#[derive(Debug, Clone)]
pub struct StashEntry {
    pub index: usize,
    pub oid: ObjectId,
    pub message: String,
}

/// Pushes current working tree and index modifications onto `refs/stash` (`gir stash push [-u] [-m <msg>]`).
pub fn stash_push(
    repo: &Repository,
    message: Option<&str>,
    include_untracked: bool,
) -> Result<(ObjectId, String), GirError> {
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let head_oid = repo.refs().resolve_to_oid("HEAD").map_err(|_| {
        GirError::InvalidArgument("You do not have the initial commit yet".to_string())
    })?;
    let (base_commit_oid, base_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;

    let branch_name = match repo.refs().read_raw("HEAD")? {
        Some(RefTarget::Symbolic(sym)) => sym
            .strip_prefix("refs/heads/")
            .unwrap_or(&sym)
            .to_string(),
        _ => "(no branch)".to_string(),
    };

    let mut index = GitIndex::read(repo.git_dir())?;
    let index_tree_oid = write_index_to_tree(&mut index, repo.odb())?;
    let head_tree_map = flatten_tree(repo.odb(), &base_commit.tree)?;

    // Build working-tree index for tracked files (union of current index and HEAD tree)
    let mut wt_index = index.clone();
    let mut tracked_paths: BTreeSet<String> = index
        .entries()
        .filter(|e| e.stage() == 0)
        .map(|e| e.path.clone())
        .collect();
    tracked_paths.extend(head_tree_map.keys().cloned());

    for rel_path in &tracked_paths {
        let abs = worktree.join(rel_path);
        match fs::symlink_metadata(&abs) {
            Ok(meta) if !meta.file_type().is_dir() => {
                let data = read_worktree_entry(&abs, &meta)?;
                let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &data)?;
                wt_index.add_entry(IndexEntry::from_metadata(rel_path, blob_oid, 0, &meta));
            }
            _ => {
                wt_index.remove_path(rel_path);
            }
        }
    }
    let wt_tree_oid = write_index_to_tree(&mut wt_index, repo.odb())?;

    // Collect untracked files if `-u` / `--include-untracked` is passed
    let mut untracked_paths = Vec::new();
    let untracked_commit_oid = if include_untracked {
        let root_ignore = IgnoreStack::for_repository(repo);
        let mut u_index = GitIndex::new();
        collect_untracked_into_index(
            repo,
            &worktree,
            "",
            &index,
            &head_tree_map,
            &root_ignore,
            &mut u_index,
            &mut untracked_paths,
        )?;
        if !u_index.is_empty() {
            let u_tree_oid = write_index_to_tree(&mut u_index, repo.odb())?;
            let sig = fallback_sig(repo);
            let u_commit = Commit {
                tree: u_tree_oid,
                parents: Vec::new(),
                author: sig.clone(),
                committer: sig,
                message: format!(
                    "untracked files on {branch_name}: {} {}\n",
                    base_commit_oid.short_hex(),
                    base_commit.subject()
                ),
            };
            Some(repo.odb().write_object(&u_commit.to_raw())?)
        } else {
            None
        }
    } else {
        None
    };

    // Check if there are any local changes to stash
    if index_tree_oid == base_commit.tree
        && wt_tree_oid == base_commit.tree
        && untracked_commit_oid.is_none()
    {
        return Err(GirError::InvalidArgument(
            "No local changes to save".to_string(),
        ));
    }

    let sig = fallback_sig(repo);

    // 1. Write P2 (`I`): commit of current `.git/index` with parent `B` (`HEAD`)
    let index_commit = Commit {
        tree: index_tree_oid,
        parents: vec![base_commit_oid],
        author: sig.clone(),
        committer: sig.clone(),
        message: format!(
            "index on {branch_name}: {} {}\n",
            base_commit_oid.short_hex(),
            base_commit.subject()
        ),
    };
    let index_commit_oid = repo.odb().write_object(&index_commit.to_raw())?;

    // 2. Write Stash Commit (`W`) with parents `[B, I]` (plus `U` if untracked files were stashed)
    let mut w_parents = vec![base_commit_oid, index_commit_oid];
    if let Some(u_oid) = untracked_commit_oid {
        w_parents.push(u_oid);
    }

    let summary_line = match message {
        Some(m) if !m.trim().is_empty() => format!("On {branch_name}: {}", m.trim()),
        _ => format!(
            "WIP on {branch_name}: {} {}",
            base_commit_oid.short_hex(),
            base_commit.subject()
        ),
    };

    let w_commit = Commit {
        tree: wt_tree_oid,
        parents: w_parents,
        author: sig.clone(),
        committer: sig.clone(),
        message: format!("{summary_line}\n"),
    };
    let stash_oid = repo.odb().write_object(&w_commit.to_raw())?;

    // 3. Update `refs/stash` and append to `.git/logs/refs/stash`
    let old_stash_oid = repo
        .refs()
        .resolve_to_oid("refs/stash")
        .unwrap_or(ObjectId::ZERO);
    let stash_ref_path = repo.git_dir().join("refs/stash");
    {
        let mut lock = LockFile::acquire(&stash_ref_path)?;
        writeln!(lock, "{stash_oid}")?;
        lock.commit()?;
    }
    append_reflog(
        repo.git_dir(),
        "refs/stash",
        old_stash_oid,
        stash_oid,
        &sig,
        &summary_line,
    )?;

    // 4. Reset working tree and index to `HEAD`, and remove stashed untracked files
    checkout_tree(repo, &base_commit.tree, true, "stash push")?;
    for rel_u in &untracked_paths {
        let abs_u = worktree.join(rel_u);
        if fs::symlink_metadata(&abs_u).is_ok() {
            let _ = fs::remove_file(&abs_u);
            prune_empty_parent_dirs(&abs_u, &worktree);
        }
    }

    Ok((stash_oid, summary_line))
}

fn collect_untracked_into_index(
    repo: &Repository,
    worktree: &Path,
    rel_dir: &str,
    tracked_index: &GitIndex,
    head_tree_map: &BTreeMap<String, (crate::object::tree::FileMode, ObjectId)>,
    ignore_stack: &IgnoreStack,
    u_index: &mut GitIndex,
    untracked_paths: &mut Vec<String>,
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
        };

        let rel_path = if rel_dir.is_empty() {
            name_str.to_string()
        } else {
            format!("{rel_dir}/{name_str}")
        };
        let abs_path = entry.path();
        let meta = fs::symlink_metadata(&abs_path)?;

        if meta.file_type().is_dir() {
            let dir_prefix = format!("{rel_path}/");
            let has_head_tracked = head_tree_map.keys().any(|k| k.starts_with(&dir_prefix));
            if !tracked_index.contains_path_or_dir(&rel_path)
                && !has_head_tracked
                && ignore_stack.is_ignored(&rel_path, true)
            {
                continue;
            }
            let child_ignore = ignore_stack.enter_dir(&rel_path, &abs_path);
            collect_untracked_into_index(
                repo,
                worktree,
                &rel_path,
                tracked_index,
                head_tree_map,
                &child_ignore,
                u_index,
                untracked_paths,
            )?;
        } else {
            if tracked_index.get(&rel_path, 0).is_some() || head_tree_map.contains_key(&rel_path) {
                continue;
            }
            if ignore_stack.is_ignored(&rel_path, false) {
                continue;
            }
            let data = read_worktree_entry(&abs_path, &meta)?;
            let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &data)?;
            u_index.add_entry(IndexEntry::from_metadata(&rel_path, blob_oid, 0, &meta));
            untracked_paths.push(rel_path);
        }
    }

    Ok(())
}

/// Lists all stashes in `.git/logs/refs/stash` from newest (`stash@{0}`) to oldest.
pub fn stash_list(repo: &Repository) -> Result<Vec<StashEntry>, GirError> {
    if repo.refs().resolve_to_oid("refs/stash").is_err() {
        return Ok(Vec::new());
    }
    let mut entries = read_reflog(repo.git_dir(), "refs/stash")?;
    entries.reverse();

    let mut out = Vec::with_capacity(entries.len());
    for (idx, e) in entries.into_iter().enumerate() {
        out.push(StashEntry {
            index: idx,
            oid: e.new_oid,
            message: e.message,
        });
    }
    Ok(out)
}

/// Resolves a stash specifier (`None`, `"0"`, `"stash@{1}"`) to its stash index `0..N` and commit `ObjectId`.
pub fn resolve_stash_spec(
    repo: &Repository,
    spec: Option<&str>,
) -> Result<(usize, ObjectId), GirError> {
    let idx: usize = match spec {
        None | Some("") => 0,
        Some(s) => {
            if let Some(inner) = s.strip_prefix("stash@{").and_then(|r| r.strip_suffix('}')) {
                inner.parse().map_err(|_| {
                    GirError::InvalidArgument(format!("'{s}' is not a valid stash reference"))
                })?
            } else {
                s.parse().map_err(|_| {
                    GirError::InvalidArgument(format!("'{s}' is not a valid stash reference"))
                })?
            }
        }
    };

    if idx == 0 {
        let log_entries = read_reflog(repo.git_dir(), "refs/stash")?;
        if let Some(last) = log_entries.last() {
            return Ok((0, last.new_oid));
        }
        let oid = repo
            .refs()
            .resolve_to_oid("refs/stash")
            .map_err(|_| GirError::InvalidArgument("No stash entries found.".to_string()))?;
        return Ok((0, oid));
    }

    let oid = resolve_reflog_at(repo.git_dir(), "refs/stash", idx)?;
    Ok((idx, oid))
}

/// Applies stash `spec` (`stash@{N}`) onto the working tree (and `.git/index` if `restore_index` is true).
pub fn stash_apply(
    repo: &Repository,
    spec: Option<&str>,
    restore_index: bool,
) -> Result<ObjectId, GirError> {
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let (_, stash_oid) = resolve_stash_spec(repo, spec)?;
    let (_, w_commit) = peel_to_commit_object(repo.odb(), &stash_oid)?;
    let Some(&base_oid) = w_commit.parents.first() else {
        return Err(GirError::InvalidArgument(
            "corrupt stash commit: missing base parent".to_string(),
        ));
    };
    let (_, base_commit) = peel_to_commit_object(repo.odb(), &base_oid)?;

    let base_map = flatten_tree(repo.odb(), &base_commit.tree)?;
    let w_map = flatten_tree(repo.odb(), &w_commit.tree)?;
    let i_map = if let Some(i_commit_oid) = w_commit.parents.get(1) {
        let (_, ic) = peel_to_commit_object(repo.odb(), i_commit_oid)?;
        flatten_tree(repo.odb(), &ic.tree)?
    } else {
        BTreeMap::new()
    };

    let u_map = if let Some(u_commit_oid) = w_commit.parents.get(2) {
        let (_, u_commit) = peel_to_commit_object(repo.odb(), u_commit_oid)?;
        flatten_tree(repo.odb(), &u_commit.tree)?
    } else {
        BTreeMap::new()
    };

    // Phase 1: Pre-validate untracked P3 collisions and pre-compute all merged blobs before touching disk
    for path in u_map.keys() {
        let abs_path = worktree.join(path);
        if fs::symlink_metadata(&abs_path).is_ok() {
            return Err(GirError::InvalidArgument(format!(
                "{path}: already exists, no checkout"
            )));
        }
    }

    enum PlannedStashOp {
        Add {
            path: String,
            mode: crate::object::tree::FileMode,
            oid: ObjectId,
            idx_oid: ObjectId,
        },
        Delete {
            path: String,
        },
        Modify {
            path: String,
            mode: crate::object::tree::FileMode,
            oid: ObjectId,
            idx_entry: Option<(crate::object::tree::FileMode, ObjectId)>,
        },
    }

    let mut all_paths: BTreeSet<&String> = BTreeSet::new();
    all_paths.extend(base_map.keys());
    all_paths.extend(w_map.keys());

    let mut planned_ops = Vec::new();

    for path in all_paths {
        let b_val = base_map.get(path).copied();
        let w_val = w_map.get(path).copied();
        if b_val == w_val {
            continue;
        }

        let abs_path = worktree.join(path);
        match (b_val, w_val) {
            (None, Some((w_mode, w_oid))) => {
                let idx_oid = if restore_index {
                    i_map.get(path).map(|&(_, id)| id).unwrap_or(w_oid)
                } else {
                    w_oid
                };
                planned_ops.push(PlannedStashOp::Add {
                    path: path.clone(),
                    mode: w_mode,
                    oid: w_oid,
                    idx_oid,
                });
            }
            (Some(_), None) => {
                planned_ops.push(PlannedStashOp::Delete { path: path.clone() });
            }
            (Some((b_mode, b_oid)), Some((w_mode, w_oid))) => {
                let wt_meta = fs::symlink_metadata(&abs_path).ok();
                let can_direct_write = match wt_meta {
                    Some(ref meta) if !meta.file_type().is_dir() => {
                        let cur_bytes = read_worktree_entry(&abs_path, meta)?;
                        let cur_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &cur_bytes);
                        cur_oid == b_oid && mode_from_metadata(meta) == b_mode
                    }
                    _ => false,
                };

                let target_oid = if can_direct_write {
                    w_oid
                } else if let Some(ref meta) = wt_meta {
                    let cur_bytes = read_worktree_entry(&abs_path, meta)?;
                    let b_bytes = repo.odb().read_raw(&b_oid)?.data;
                    let w_bytes = repo.odb().read_raw(&w_oid)?.data;
                    let (is_bin, hunks) = build_hunks_from_bytes(&b_bytes, &w_bytes, 3);
                    if is_bin {
                        w_oid
                    } else {
                        let hunk_refs: Vec<&_> = hunks.iter().collect();
                        let merged_bytes = apply_hunks_to_blob(&cur_bytes, &hunk_refs)?;
                        repo.odb().write_raw(ObjectKind::Blob, &merged_bytes)?
                    }
                } else {
                    w_oid
                };

                let idx_entry = if restore_index {
                    i_map.get(path).copied()
                } else {
                    None
                };

                planned_ops.push(PlannedStashOp::Modify {
                    path: path.clone(),
                    mode: w_mode,
                    oid: target_oid,
                    idx_entry,
                });
            }
            (None, None) => {}
        }
    }

    // Phase 2: Capture safety snapshot and materialize all planned operations
    let _ = SafetySnapshotEngine::capture(repo, "stash apply");
    let mut index = GitIndex::read(repo.git_dir())?;

    for op in planned_ops {
        match op {
            PlannedStashOp::Add {
                path,
                mode,
                oid,
                idx_oid,
            } => {
                let abs_path = worktree.join(&path);
                materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, mode, &oid)?;
                let meta = fs::symlink_metadata(&abs_path)?;
                index.add_entry(IndexEntry::from_metadata(&path, idx_oid, 0, &meta));
            }
            PlannedStashOp::Delete { path } => {
                let abs_path = worktree.join(&path);
                if fs::symlink_metadata(&abs_path).is_ok() {
                    let _ = fs::remove_file(&abs_path);
                    prune_empty_parent_dirs(&abs_path, &worktree);
                }
                index.remove_path(&path);
            }
            PlannedStashOp::Modify {
                path,
                mode,
                oid,
                idx_entry,
            } => {
                let abs_path = worktree.join(&path);
                materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, mode, &oid)?;
                if let Some((i_mode, i_oid)) = idx_entry {
                    index.add_entry(IndexEntry::from_tree_entry(path, i_mode, i_oid, 0));
                }
            }
        }
    }

    for (path, (u_mode, u_oid)) in u_map {
        let abs_path = worktree.join(&path);
        materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, u_mode, &u_oid)?;
    }

    let _ = write_index_to_tree(&mut index, repo.odb());
    index.write(repo.git_dir())?;
    Ok(stash_oid)
}

/// Drops a single stash entry (`stash@{N}`) from `.git/logs/refs/stash` and updates `refs/stash`.
pub fn stash_drop(repo: &Repository, spec: Option<&str>) -> Result<(usize, ObjectId), GirError> {
    let (idx, dropped_oid) = resolve_stash_spec(repo, spec)?;
    let mut entries = read_reflog(repo.git_dir(), "refs/stash")?;

    if entries.is_empty() {
        let _ = fs::remove_file(repo.git_dir().join("refs/stash"));
        return Ok((idx, dropped_oid));
    }

    if idx >= entries.len() {
        return Err(GirError::InvalidArgument(format!(
            "stash@{{{idx}}} does not exist"
        )));
    }

    let vec_pos = entries.len() - 1 - idx;
    entries.remove(vec_pos);

    if entries.is_empty() {
        let _ = fs::remove_file(repo.git_dir().join("refs/stash"));
        let _ = fs::remove_file(repo.git_dir().join("logs/refs/stash"));
    } else {
        // Re-link `old_oid` pointers in the remaining reflog chain and update `refs/stash` to the top entry
        let mut prev_oid = ObjectId::ZERO;
        for entry in &mut entries {
            entry.old_oid = prev_oid;
            prev_oid = entry.new_oid;
        }
        let top_oid = entries.last().unwrap().new_oid;

        let stash_ref_path = repo.git_dir().join("refs/stash");
        {
            let mut lock = LockFile::acquire(&stash_ref_path)?;
            writeln!(lock, "{top_oid}")?;
            lock.commit()?;
        }

        let log_path = repo.git_dir().join("logs/refs/stash");
        {
            let mut lock = LockFile::acquire(&log_path)?;
            for e in &entries {
                lock.write_all(e.serialize().as_bytes())?;
            }
            lock.commit()?;
        }
    }

    Ok((idx, dropped_oid))
}

/// Applies stash `spec` and immediately drops it if application succeeded (`gir stash pop`).
pub fn stash_pop(
    repo: &Repository,
    spec: Option<&str>,
    restore_index: bool,
) -> Result<(usize, ObjectId), GirError> {
    let _ = stash_apply(repo, spec, restore_index)?;
    stash_drop(repo, spec)
}

/// Removes all stash entries (`gir stash clear`).
pub fn stash_clear(repo: &Repository) -> Result<(), GirError> {
    let _ = fs::remove_file(repo.git_dir().join("refs/stash"));
    let _ = fs::remove_file(repo.git_dir().join("logs/refs/stash"));
    Ok(())
}
