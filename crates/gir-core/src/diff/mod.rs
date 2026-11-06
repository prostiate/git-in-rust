pub mod hunk;
pub mod myers;
pub mod patch;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use crate::commit_ops::{FileChangeKind, FileChangeStat, TreeDiffSummary};
use crate::error::GirError;
use crate::hash::ObjectId;
use crate::index::{flatten_tree, GitIndex};
use crate::object::tree::FileMode;
use crate::object::ObjectKind;
use crate::repository::Repository;
use crate::storage::ObjectDatabase;

pub use hunk::{build_hunks_from_bytes, DiffHunk, FilePatch, HunkLine};
pub use myers::{myers_diff, EditOp};
pub use patch::apply_hunks_to_blob;

/// Converts a slice of `FilePatch` items into a `TreeDiffSummary` (for `--stat` / `--shortstat`).
pub fn summarize_patches(patches: &[FilePatch]) -> TreeDiffSummary {
    let mut files = Vec::with_capacity(patches.len());
    let mut total_insertions = 0usize;
    let mut total_deletions = 0usize;

    for p in patches {
        let ins = p.insertions();
        let del = p.deletions();
        total_insertions += ins;
        total_deletions += del;
        files.push(FileChangeStat {
            path: p.path.clone(),
            kind: p.kind,
            old_mode: p.old_mode,
            new_mode: p.new_mode,
            old_oid: p.old_oid,
            new_oid: p.new_oid,
            insertions: ins,
            deletions: del,
            is_binary: p.is_binary,
        });
    }

    TreeDiffSummary {
        files,
        total_insertions,
        total_deletions,
    }
}

/// Computes `FilePatch` diffs between two `Tree` snapshots (`old_tree` -> `new_tree`).
pub fn diff_tree_to_tree(
    odb: &ObjectDatabase,
    old_tree: Option<&ObjectId>,
    new_tree: &ObjectId,
    pathspecs: &[String],
) -> Result<Vec<FilePatch>, GirError> {
    let old_map = match old_tree {
        Some(oid) => flatten_tree(odb, oid)?,
        None => BTreeMap::new(),
    };
    let new_map = flatten_tree(odb, new_tree)?;

    diff_entry_maps(odb, &old_map, &new_map, pathspecs)
}

/// Computes `FilePatch` diffs between a `Tree` snapshot (typically `HEAD^{tree}`) and `.git/index` (`gir diff --staged`).
pub fn diff_tree_to_index(
    odb: &ObjectDatabase,
    old_tree: Option<&ObjectId>,
    index: &GitIndex,
    pathspecs: &[String],
) -> Result<Vec<FilePatch>, GirError> {
    let old_map = match old_tree {
        Some(oid) => flatten_tree(odb, oid)?,
        None => BTreeMap::new(),
    };

    let mut index_map = BTreeMap::new();
    for entry in index.entries() {
        if entry.stage() == 0 {
            index_map.insert(entry.path.clone(), (entry.file_mode(), entry.oid));
        }
    }

    diff_entry_maps(odb, &old_map, &index_map, pathspecs)
}

/// Computes `FilePatch` diffs between `.git/index` and the working tree (`gir diff`).
pub fn diff_index_to_worktree(
    repo: &Repository,
    index: &GitIndex,
    pathspecs: &[String],
) -> Result<Vec<FilePatch>, GirError> {
    let Some(worktree) = repo.worktree() else {
        return Ok(Vec::new());
    };

    let mut patches = Vec::new();
    for entry in index.entries() {
        if entry.stage() != 0 {
            continue;
        }
        if !matches_pathspecs(&entry.path, pathspecs) {
            continue;
        }

        let abs_path = worktree.join(&entry.path);
        match fs::symlink_metadata(&abs_path) {
            Ok(meta) if !meta.file_type().is_dir() => {
                let wt_mode = mode_from_metadata(&meta);
                let wt_bytes = read_worktree_entry(&abs_path, &meta)?;
                let wt_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &wt_bytes);

                let idx_mode = entry.file_mode();
                if wt_oid == entry.oid && wt_mode == idx_mode {
                    continue;
                }

                let idx_bytes = repo.odb().read_raw(&entry.oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&idx_bytes, &wt_bytes, 3);
                patches.push(FilePatch {
                    path: entry.path.clone(),
                    kind: FileChangeKind::Modified,
                    old_mode: Some(idx_mode),
                    new_mode: Some(wt_mode),
                    old_oid: Some(entry.oid),
                    new_oid: Some(wt_oid),
                    is_binary,
                    hunks,
                });
            }
            _ => {
                // Deleted in working tree
                let idx_bytes = repo.odb().read_raw(&entry.oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&idx_bytes, &[], 3);
                patches.push(FilePatch {
                    path: entry.path.clone(),
                    kind: FileChangeKind::Deleted,
                    old_mode: Some(entry.file_mode()),
                    new_mode: None,
                    old_oid: Some(entry.oid),
                    new_oid: None,
                    is_binary,
                    hunks,
                });
            }
        }
    }

    Ok(patches)
}

/// Computes `FilePatch` diffs between a `Tree` snapshot and the working tree (`gir diff <commit>`).
pub fn diff_tree_to_worktree(
    repo: &Repository,
    tree_oid: &ObjectId,
    index: &GitIndex,
    pathspecs: &[String],
) -> Result<Vec<FilePatch>, GirError> {
    let Some(worktree) = repo.worktree() else {
        return Ok(Vec::new());
    };

    let tree_map = flatten_tree(repo.odb(), tree_oid)?;
    let mut candidate_paths: BTreeSet<String> = tree_map.keys().cloned().collect();
    for entry in index.entries() {
        if entry.stage() == 0 {
            candidate_paths.insert(entry.path.clone());
        }
    }

    let mut patches = Vec::new();
    for path in candidate_paths {
        if !matches_pathspecs(&path, pathspecs) {
            continue;
        }
        let old_entry = tree_map.get(&path).copied();
        let abs_path = worktree.join(&path);
        let wt_meta = fs::symlink_metadata(&abs_path)
            .ok()
            .filter(|m| !m.file_type().is_dir());

        match (old_entry, wt_meta) {
            (None, Some(meta)) => {
                let wt_mode = mode_from_metadata(&meta);
                let wt_bytes = read_worktree_entry(&abs_path, &meta)?;
                let wt_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &wt_bytes);
                let (is_binary, hunks) = build_hunks_from_bytes(&[], &wt_bytes, 3);
                patches.push(FilePatch {
                    path,
                    kind: FileChangeKind::Added,
                    old_mode: None,
                    new_mode: Some(wt_mode),
                    old_oid: None,
                    new_oid: Some(wt_oid),
                    is_binary,
                    hunks,
                });
            }
            (Some((old_mode, old_oid)), None) => {
                let old_bytes = repo.odb().read_raw(&old_oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&old_bytes, &[], 3);
                patches.push(FilePatch {
                    path,
                    kind: FileChangeKind::Deleted,
                    old_mode: Some(old_mode),
                    new_mode: None,
                    old_oid: Some(old_oid),
                    new_oid: None,
                    is_binary,
                    hunks,
                });
            }
            (Some((old_mode, old_oid)), Some(meta)) => {
                let wt_mode = mode_from_metadata(&meta);
                let wt_bytes = read_worktree_entry(&abs_path, &meta)?;
                let wt_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &wt_bytes);
                if old_oid == wt_oid && old_mode == wt_mode {
                    continue;
                }
                let old_bytes = repo.odb().read_raw(&old_oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&old_bytes, &wt_bytes, 3);
                patches.push(FilePatch {
                    path,
                    kind: FileChangeKind::Modified,
                    old_mode: Some(old_mode),
                    new_mode: Some(wt_mode),
                    old_oid: Some(old_oid),
                    new_oid: Some(wt_oid),
                    is_binary,
                    hunks,
                });
            }
            (None, None) => {}
        }
    }

    Ok(patches)
}

fn diff_entry_maps(
    odb: &ObjectDatabase,
    old_map: &BTreeMap<String, (FileMode, ObjectId)>,
    new_map: &BTreeMap<String, (FileMode, ObjectId)>,
    pathspecs: &[String],
) -> Result<Vec<FilePatch>, GirError> {
    let mut all_paths: BTreeSet<&String> = BTreeSet::new();
    all_paths.extend(old_map.keys());
    all_paths.extend(new_map.keys());

    let mut patches = Vec::new();
    for path in all_paths {
        if !matches_pathspecs(path, pathspecs) {
            continue;
        }
        let old_entry = old_map.get(path).copied();
        let new_entry = new_map.get(path).copied();

        match (old_entry, new_entry) {
            (None, Some((new_mode, new_oid))) => {
                let new_bytes = odb.read_raw(&new_oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&[], &new_bytes, 3);
                patches.push(FilePatch {
                    path: path.clone(),
                    kind: FileChangeKind::Added,
                    old_mode: None,
                    new_mode: Some(new_mode),
                    old_oid: None,
                    new_oid: Some(new_oid),
                    is_binary,
                    hunks,
                });
            }
            (Some((old_mode, old_oid)), None) => {
                let old_bytes = odb.read_raw(&old_oid)?.data;
                let (is_binary, hunks) = build_hunks_from_bytes(&old_bytes, &[], 3);
                patches.push(FilePatch {
                    path: path.clone(),
                    kind: FileChangeKind::Deleted,
                    old_mode: Some(old_mode),
                    new_mode: None,
                    old_oid: Some(old_oid),
                    new_oid: None,
                    is_binary,
                    hunks,
                });
            }
            (Some((old_mode, old_oid)), Some((new_mode, new_oid))) => {
                if old_mode == new_mode && old_oid == new_oid {
                    continue;
                }
                let (is_binary, hunks) = if old_oid == new_oid {
                    (false, Vec::new())
                } else {
                    let old_bytes = odb.read_raw(&old_oid)?.data;
                    let new_bytes = odb.read_raw(&new_oid)?.data;
                    build_hunks_from_bytes(&old_bytes, &new_bytes, 3)
                };
                patches.push(FilePatch {
                    path: path.clone(),
                    kind: FileChangeKind::Modified,
                    old_mode: Some(old_mode),
                    new_mode: Some(new_mode),
                    old_oid: Some(old_oid),
                    new_oid: Some(new_oid),
                    is_binary,
                    hunks,
                });
            }
            (None, None) => {}
        }
    }

    Ok(patches)
}

pub fn matches_pathspecs(path: &str, pathspecs: &[String]) -> bool {
    if pathspecs.is_empty() {
        return true;
    }
    pathspecs.iter().any(|spec| {
        let clean = spec.trim_matches('/');
        clean.is_empty() || path == clean || path.starts_with(&format!("{clean}/"))
    })
}

pub fn mode_from_metadata(meta: &fs::Metadata) -> FileMode {
    use std::os::unix::fs::MetadataExt;
    if meta.file_type().is_symlink() {
        FileMode::Symlink
    } else if (meta.mode() & 0o111) != 0 {
        FileMode::Executable
    } else {
        FileMode::Regular
    }
}

pub fn read_worktree_entry(abs_path: &Path, meta: &fs::Metadata) -> Result<Vec<u8>, GirError> {
    if meta.file_type().is_symlink() {
        Ok(fs::read_link(abs_path)?
            .to_string_lossy()
            .into_owned()
            .into_bytes())
    } else {
        Ok(fs::read(abs_path)?)
    }
}
