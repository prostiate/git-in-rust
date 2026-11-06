use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::diff::{mode_from_metadata, read_worktree_entry};
use crate::error::GirError;
use crate::hash::ObjectId;
use crate::index::{flatten_tree, write_index_to_tree, GitIndex, IndexEntry};
use crate::merge::file_merge::{merge_blobs_3way, ConflictStyle, MergeLabels};
use crate::object::tree::FileMode;
use crate::object::ObjectKind;
use crate::repository::Repository;
use crate::storage::atomic::LockFile;
use crate::storage::ObjectDatabase;
use crate::worktree::{
    dir_contains_untracked_file, materialize_blob_to_worktree, prune_empty_parent_dirs,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MergedPathState {
    /// Path resolves cleanly to `(mode, oid)` at Stage 0.
    Clean {
        mode: FileMode,
        oid: ObjectId,
    },
    /// Path is cleanly deleted from both index and working tree.
    Deleted,
    /// Path has a 3-way conflict with optional stages `1 (base)`, `2 (ours)`, `3 (theirs)`
    /// and `worktree_bytes` (containing `<<<<<<<` / `=======` / `>>>>>>>` conflict markers) to write on disk.
    Conflict {
        base: Option<(FileMode, ObjectId)>,
        ours: Option<(FileMode, ObjectId)>,
        theirs: Option<(FileMode, ObjectId)>,
        worktree_mode: FileMode,
        worktree_bytes: Vec<u8>,
        description: String,
    },
}

#[derive(Debug, Clone)]
pub struct TreeMergeOutcome {
    pub paths: BTreeMap<String, MergedPathState>,
    pub conflicts: Vec<String>,
}

impl TreeMergeOutcome {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

/// Computes a 3-way tree merge between `base_tree` ($B$, optional for unrelated/root histories),
/// `ours_tree` ($O$), and `theirs_tree` ($T$).
pub fn merge_trees_3way(
    odb: &ObjectDatabase,
    base_tree: Option<&ObjectId>,
    ours_tree: &ObjectId,
    theirs_tree: &ObjectId,
    labels: MergeLabels<'_>,
    style: ConflictStyle,
) -> Result<TreeMergeOutcome, GirError> {
    let base_map = match base_tree {
        Some(oid) => flatten_tree(odb, oid)?,
        None => BTreeMap::new(),
    };
    let ours_map = flatten_tree(odb, ours_tree)?;
    let theirs_map = flatten_tree(odb, theirs_tree)?;

    let mut all_paths: BTreeSet<String> = BTreeSet::new();
    all_paths.extend(base_map.keys().cloned());
    all_paths.extend(ours_map.keys().cloned());
    all_paths.extend(theirs_map.keys().cloned());

    let mut paths = BTreeMap::new();
    let mut conflicts = Vec::new();

    for path in all_paths {
        let b = base_map.get(&path).copied();
        let o = ours_map.get(&path).copied();
        let t = theirs_map.get(&path).copied();

        match (b, o, t) {
            // Identical in Ours and Theirs (including when both modified identically or both deleted)
            (_, Some((o_mode, o_oid)), Some((t_mode, t_oid)))
                if o_mode == t_mode && o_oid == t_oid =>
            {
                paths.insert(
                    path,
                    MergedPathState::Clean {
                        mode: o_mode,
                        oid: o_oid,
                    },
                );
            }
            (_, None, None) => {
                paths.insert(path, MergedPathState::Deleted);
            }

            // Unchanged in Ours (`B == O`), modified or deleted in Theirs -> take Theirs
            (Some(b_val), Some(o_val), Some((t_mode, t_oid))) if b_val == o_val => {
                paths.insert(
                    path,
                    MergedPathState::Clean {
                        mode: t_mode,
                        oid: t_oid,
                    },
                );
            }
            (Some(b_val), Some(o_val), None) if b_val == o_val => {
                paths.insert(path, MergedPathState::Deleted);
            }
            (None, None, Some((t_mode, t_oid))) => {
                paths.insert(
                    path,
                    MergedPathState::Clean {
                        mode: t_mode,
                        oid: t_oid,
                    },
                );
            }

            // Unchanged in Theirs (`B == T`), modified or deleted in Ours -> keep Ours
            (Some(b_val), Some((o_mode, o_oid)), Some(t_val)) if b_val == t_val => {
                paths.insert(
                    path,
                    MergedPathState::Clean {
                        mode: o_mode,
                        oid: o_oid,
                    },
                );
            }
            (Some(b_val), None, Some(t_val)) if b_val == t_val => {
                paths.insert(path, MergedPathState::Deleted);
            }
            (None, Some((o_mode, o_oid)), None) => {
                paths.insert(
                    path,
                    MergedPathState::Clean {
                        mode: o_mode,
                        oid: o_oid,
                    },
                );
            }

            // Modify/Delete conflict: modified in Ours (`O != B`), deleted in Theirs (`T == None`)
            (Some(b_val), Some((o_mode, o_oid)), None) => {
                let wt_bytes = odb.read_raw(&o_oid)?.data;
                let desc = format!(
                    "CONFLICT (modify/delete): {path} deleted in {} and modified in {}. Version {} of {path} left in tree.",
                    labels.theirs, labels.ours, labels.ours
                );
                conflicts.push(path.clone());
                paths.insert(
                    path,
                    MergedPathState::Conflict {
                        base: Some(b_val),
                        ours: Some((o_mode, o_oid)),
                        theirs: None,
                        worktree_mode: o_mode,
                        worktree_bytes: wt_bytes,
                        description: desc,
                    },
                );
            }

            // Delete/Modify conflict: deleted in Ours (`O == None`), modified in Theirs (`T != B`)
            (Some(b_val), None, Some((t_mode, t_oid))) => {
                let wt_bytes = odb.read_raw(&t_oid)?.data;
                let desc = format!(
                    "CONFLICT (modify/delete): {path} deleted in {} and modified in {}. Version {} of {path} left in tree.",
                    labels.ours, labels.theirs, labels.theirs
                );
                conflicts.push(path.clone());
                paths.insert(
                    path,
                    MergedPathState::Conflict {
                        base: Some(b_val),
                        ours: None,
                        theirs: Some((t_mode, t_oid)),
                        worktree_mode: t_mode,
                        worktree_bytes: wt_bytes,
                        description: desc,
                    },
                );
            }

            // Both modified (`B = Some`, `O = Some`, `T = Some`) or both added (`B = None`, `O = Some`, `T = Some`)
            (b_opt, Some((o_mode, o_oid)), Some((t_mode, t_oid))) => {
                let base_bytes = match b_opt {
                    Some((_, b_oid)) => odb.read_raw(&b_oid)?.data,
                    None => Vec::new(),
                };
                let ours_bytes = odb.read_raw(&o_oid)?.data;
                let theirs_bytes = odb.read_raw(&t_oid)?.data;

                let merged_mode = if o_mode == t_mode {
                    o_mode
                } else if b_opt.map(|(bm, _)| bm) == Some(o_mode) {
                    t_mode
                } else {
                    o_mode
                };

                let file_res = merge_blobs_3way(&base_bytes, &ours_bytes, &theirs_bytes, labels.clone(), style);
                if !file_res.has_conflicts {
                    let merged_oid = odb.write_raw(ObjectKind::Blob, &file_res.content)?;
                    paths.insert(
                        path,
                        MergedPathState::Clean {
                            mode: merged_mode,
                            oid: merged_oid,
                        },
                    );
                } else {
                    let kind_label = if b_opt.is_some() { "content" } else { "add/add" };
                    let desc = format!("CONFLICT ({kind_label}): Merge conflict in {path}");
                    conflicts.push(path.clone());
                    paths.insert(
                        path,
                        MergedPathState::Conflict {
                            base: b_opt,
                            ours: Some((o_mode, o_oid)),
                            theirs: Some((t_mode, t_oid)),
                            worktree_mode: merged_mode,
                            worktree_bytes: file_res.content,
                            description: desc,
                        },
                    );
                }
            }
        }
    }

    Ok(TreeMergeOutcome { paths, conflicts })
}

/// Validates that the working tree and index have no conflicting dirty/untracked files on paths
/// touched by `outcome`, then applies `outcome` to `.git/index` and the working tree.
pub fn apply_tree_merge_to_worktree(
    repo: &Repository,
    ours_tree: &ObjectId,
    outcome: &TreeMergeOutcome,
) -> Result<(), GirError> {
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let ours_map = flatten_tree(repo.odb(), ours_tree)?;
    let mut index = GitIndex::read(repo.git_dir())?;

    if index.has_conflicts() {
        return Err(GirError::InvalidArgument(
            "Merging is not possible because you have unmerged files.\nhint: Fix them up in the work tree, and then use 'gir add/rm <file>'\nhint: as appropriate to mark resolution and make a commit.\nExiting because of an unresolved conflict.".to_string(),
        ));
    }

    // Phase 1: Pre-validate that any path changing relative to `ours_map` has no local dirty or untracked collision
    let mut dirty_conflicts = Vec::new();
    let mut untracked_conflicts = Vec::new();

    for (path, merged_state) in &outcome.paths {
        let o_val = ours_map.get(path).copied();
        let is_unchanged_from_ours = matches!(
            (o_val, merged_state),
            (Some((o_mode, o_oid)), MergedPathState::Clean { mode, oid })
                if o_mode == *mode && o_oid == *oid
        ) || (o_val.is_none() && matches!(merged_state, MergedPathState::Deleted));

        if is_unchanged_from_ours {
            continue;
        }

        let i_entry = index.get(path, 0);
        let i_val = i_entry.map(|e| (e.file_mode(), e.oid));
        if i_val != o_val {
            dirty_conflicts.push(path.clone());
            continue;
        }

        let abs_path = worktree.join(path);
        let wt_meta = fs::symlink_metadata(&abs_path).ok();

        if !matches!(merged_state, MergedPathState::Deleted) {
            let mut ancestor_conflict = None;
            let mut curr_rel = Path::new(path.as_str()).parent();
            while let Some(anc) = curr_rel {
                let anc_str = anc.to_string_lossy();
                if anc_str.is_empty() {
                    break;
                }
                let anc_abs = worktree.join(anc);
                if let Ok(anc_meta) = fs::symlink_metadata(&anc_abs) {
                    if !anc_meta.file_type().is_dir() {
                        let anc_key = anc_str.into_owned();
                        if !ours_map.contains_key(&anc_key) && index.get(&anc_key, 0).is_none() {
                            ancestor_conflict = Some(anc_key);
                            break;
                        }
                    }
                }
                curr_rel = anc.parent();
            }
            if let Some(anc_key) = ancestor_conflict {
                if !untracked_conflicts.contains(&anc_key) {
                    untracked_conflicts.push(anc_key);
                }
                continue;
            }
        }

        if let Some(ie) = i_entry {
            if let Some(ref meta) = wt_meta {
                if meta.file_type().is_dir() {
                    if dir_contains_untracked_file(&abs_path, path, &ours_map, &index) {
                        untracked_conflicts.push(path.clone());
                    } else {
                        dirty_conflicts.push(path.clone());
                    }
                    continue;
                }
                let wt_mode = mode_from_metadata(meta);
                let wt_bytes = read_worktree_entry(&abs_path, meta)?;
                let wt_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &wt_bytes);
                if (wt_mode, wt_oid) != (ie.file_mode(), ie.oid) {
                    dirty_conflicts.push(path.clone());
                    continue;
                }
            } else {
                dirty_conflicts.push(path.clone());
                continue;
            }
        } else if let Some(ref meta) = wt_meta {
            if meta.file_type().is_dir() {
                if dir_contains_untracked_file(&abs_path, path, &ours_map, &index) {
                    untracked_conflicts.push(path.clone());
                    continue;
                }
            } else {
                untracked_conflicts.push(path.clone());
                continue;
            }
        }
    }

    if !dirty_conflicts.is_empty() {
        return Err(GirError::InvalidArgument(format!(
            "Your local changes to the following files would be overwritten by merge:\n\t{}\nPlease commit your changes or stash them before you merge.\nAborting",
            dirty_conflicts.join("\n\t")
        )));
    }

    if !untracked_conflicts.is_empty() {
        return Err(GirError::InvalidArgument(format!(
            "The following untracked working tree files would be overwritten by merge:\n\t{}\nPlease move or remove them before you merge.\nAborting",
            untracked_conflicts.join("\n\t")
        )));
    }

    // Phase 2: Materialize changes into working tree and `.git/index`
    // Step 2a: Delete paths marked `Deleted`
    for (path, state) in &outcome.paths {
        if matches!(state, MergedPathState::Deleted) {
            let abs_path = worktree.join(path);
            if fs::symlink_metadata(&abs_path).is_ok() {
                let _ = fs::remove_file(&abs_path);
                prune_empty_parent_dirs(&abs_path, &worktree);
            }
            index.remove_path(path);
        }
    }

    // Step 2b: Write `Clean` and `Conflict` paths
    for (path, state) in &outcome.paths {
        let abs_path = worktree.join(path);
        match state {
            MergedPathState::Clean { mode, oid } => {
                let o_val = ours_map.get(path).copied();
                if o_val != Some((*mode, *oid)) {
                    materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, *mode, oid)?;
                }
                if let Ok(meta) = fs::symlink_metadata(&abs_path) {
                    index.add_entry(IndexEntry::from_metadata(path, *oid, 0, &meta));
                } else {
                    index.add_entry(IndexEntry::from_tree_entry(path.clone(), *mode, *oid, 0));
                }
            }
            MergedPathState::Conflict {
                base,
                ours,
                theirs,
                worktree_mode,
                worktree_bytes,
                ..
            } => {
                if let Some(parent) = abs_path.parent() {
                    fs::create_dir_all(parent)?;
                }
                let mut lock = LockFile::acquire(&abs_path)?;
                lock.write_all(worktree_bytes)?;
                lock.commit()?;
                let perm = if *worktree_mode == FileMode::Executable {
                    0o755
                } else {
                    0o644
                };
                let _ = fs::set_permissions(&abs_path, fs::Permissions::from_mode(perm));

                // Replace stage 0 with stages 1 (base), 2 (ours), 3 (theirs)
                index.set_conflict_stages(path, *base, *ours, *theirs);
            }
            MergedPathState::Deleted => {}
        }
    }

    if !outcome.has_conflicts() {
        let _ = write_index_to_tree(&mut index, repo.odb());
    }
    index.write(repo.git_dir())?;
    Ok(())
}
