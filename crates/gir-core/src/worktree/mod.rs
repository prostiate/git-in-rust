pub mod reset;
pub mod restore;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::branch_ops::validate_ref_name;
use crate::diff::{mode_from_metadata, read_worktree_entry};
use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::index::{flatten_tree, write_index_to_tree, GitIndex, IndexEntry};
use crate::object::commit::Signature;
use crate::object::tree::FileMode;
use crate::object::{ObjectKind, ParsedObject};
use crate::reflog::append_reflog;
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::storage::atomic::LockFile;

pub use reset::{reset_head, reset_paths, ResetMode};
pub use restore::{clean_worktree, restore_paths, CleanOptions, RestoreOptions};

#[derive(Debug, Clone, Default)]
pub struct SwitchOptions {
    pub target: Option<String>,
    pub create_branch: Option<String>,
    pub force_create: bool,
    pub detach: bool,
    pub force: bool,
}

#[derive(Debug, Clone)]
pub struct SwitchOutcome {
    pub previous_desc: String,
    pub new_branch: Option<String>,
    pub target_commit: ObjectId,
    pub was_created: bool,
    pub is_detached: bool,
}

/// Switches or checks out a branch or commit (`gir switch` / `gir checkout <target>`),
/// enforcing the Two-Tree Safe Checkout Matrix before touching any working tree file.
pub fn switch_or_checkout(
    repo: &Repository,
    opts: &SwitchOptions,
) -> Result<SwitchOutcome, GirError> {
    let head_raw = repo.refs().read_raw("HEAD")?;
    let previous_desc = match head_raw {
        Some(RefTarget::Symbolic(ref sym)) => sym
            .strip_prefix("refs/heads/")
            .unwrap_or(sym)
            .to_string(),
        Some(RefTarget::Direct(oid)) => oid.short_hex(),
        None => "HEAD".to_string(),
    };

    // Resolve `@{-1}` or `-` shorthand for previous branch
    let resolved_target_str = match opts.target.as_deref() {
        Some("-") => Some(
            crate::reflog::resolve_previous_checkout_branch(repo.git_dir(), 1)?,
        ),
        Some(other) => Some(other.to_string()),
        None => None,
    };

    // Determine target commit and branch reference
    let (target_commit_oid, target_tree_oid, branch_to_set, was_created, is_detached) =
        if let Some(ref new_branch) = opts.create_branch {
            validate_ref_name(new_branch)?;
            let full_branch_ref = format!("refs/heads/{new_branch}");
            if !opts.force_create && repo.refs().resolve_to_oid(&full_branch_ref).is_ok() {
                return Err(GirError::InvalidArgument(format!(
                    "a branch named '{new_branch}' already exists"
                )));
            }

            let start_spec = resolved_target_str.as_deref().unwrap_or("HEAD");
            let raw_oid = parse_revision(repo, start_spec)?;
            let (c_oid, commit) = peel_to_commit_object(repo.odb(), &raw_oid)?;
            (
                c_oid,
                commit.tree,
                Some((new_branch.clone(), full_branch_ref)),
                true,
                false,
            )
        } else {
            let Some(ref target_spec) = resolved_target_str else {
                return Err(GirError::InvalidArgument(
                    "missing branch or commit argument".to_string(),
                ));
            };

            let local_branch_ref = format!("refs/heads/{target_spec}");
            if !opts.detach && repo.refs().resolve_to_oid(&local_branch_ref).is_ok() {
                let raw_oid = repo.refs().resolve_to_oid(&local_branch_ref)?;
                let (c_oid, commit) = peel_to_commit_object(repo.odb(), &raw_oid)?;
                (
                    c_oid,
                    commit.tree,
                    Some((target_spec.clone(), local_branch_ref)),
                    false,
                    false,
                )
            } else {
                let raw_oid = parse_revision(repo, target_spec)?;
                let (c_oid, commit) = peel_to_commit_object(repo.odb(), &raw_oid)?;
                (c_oid, commit.tree, None, false, true)
            }
        };

    // Transition working tree and `.git/index` from `HEAD` tree to `target_tree_oid`
    let snap_label = format!(
        "checkout {}",
        opts.create_branch
            .as_deref()
            .or(resolved_target_str.as_deref())
            .unwrap_or("HEAD")
    );
    checkout_tree(repo, &target_tree_oid, opts.force, &snap_label)?;

    let sig = fallback_sig(repo);
    let old_head_oid = repo
        .refs()
        .resolve_to_oid("HEAD")
        .unwrap_or(ObjectId::ZERO);

    let new_branch_name = if let Some((short_name, full_ref)) = branch_to_set {
        if was_created {
            let expected = if opts.force_create {
                None
            } else {
                Some(ObjectId::ZERO)
            };
            let start_label = resolved_target_str.as_deref().unwrap_or("HEAD");
            repo.refs().update_ref(
                &full_ref,
                target_commit_oid,
                expected,
                true,
                &sig,
                &format!("branch: Created from {start_label}"),
            )?;
        }

        let reflog_msg = format!("checkout: moving from {previous_desc} to {short_name}");
        repo.refs()
            .set_symbolic_ref("HEAD", &full_ref, Some(&sig), Some(&reflog_msg))?;
        Some(short_name)
    } else {
        let target_label = resolved_target_str
            .as_deref()
            .unwrap_or("HEAD");
        let reflog_msg = format!("checkout: moving from {previous_desc} to {target_label}");
        let head_path = repo.git_dir().join("HEAD");
        let mut lock = LockFile::acquire(&head_path)?;
        writeln!(lock, "{target_commit_oid}")?;
        lock.commit()?;
        append_reflog(
            repo.git_dir(),
            "HEAD",
            old_head_oid,
            target_commit_oid,
            &sig,
            &reflog_msg,
        )?;
        None
    };

    Ok(SwitchOutcome {
        previous_desc,
        new_branch: new_branch_name,
        target_commit: target_commit_oid,
        was_created,
        is_detached,
    })
}

/// Safely transitions the working tree and `.git/index` from the current `HEAD` tree $H$
/// to `target_tree_oid` ($T$).
///
/// Phase 1 validates all paths against the Two-Tree Safe Checkout Matrix.
/// Phase 2 captures a pre-operation safety snapshot and applies the changes.
pub fn checkout_tree(
    repo: &Repository,
    target_tree_oid: &ObjectId,
    force: bool,
    snapshot_label: &str,
) -> Result<(), GirError> {
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let head_tree_map = match repo.refs().resolve_to_oid("HEAD") {
        Ok(head_oid) => match peel_to_commit_object(repo.odb(), &head_oid) {
            Ok((_, c)) => flatten_tree(repo.odb(), &c.tree)?,
            Err(_) => BTreeMap::new(),
        },
        Err(_) => BTreeMap::new(),
    };

    let target_tree_map = flatten_tree(repo.odb(), target_tree_oid)?;
    let mut index = GitIndex::read(repo.git_dir())?;

    let mut all_paths: BTreeSet<String> = BTreeSet::new();
    all_paths.extend(head_tree_map.keys().cloned());
    all_paths.extend(target_tree_map.keys().cloned());
    for entry in index.entries() {
        all_paths.insert(entry.path.clone());
    }

    // Phase 1: Pre-Validation (when `!force`)
    if !force {
        let mut dirty_tracked_conflicts = Vec::new();
        let mut untracked_overwrite_conflicts = Vec::new();

        for path in &all_paths {
            let h_val = head_tree_map.get(path).copied();
            let t_val = target_tree_map.get(path).copied();
            let i_entry = index.get(path, 0);
            let i_val = i_entry.map(|e| (e.file_mode(), e.oid));

            // If `H == T`, local changes (in index or worktree) carry over untouched!
            if h_val == t_val {
                continue;
            }

            // `H != T`: path is being added, modified, or deleted by the target tree
            // 1. Check if staged index entry differs from both H and T
            if i_val != h_val && i_val != t_val {
                dirty_tracked_conflicts.push(path.clone());
                continue;
            }

            let abs_path = worktree.join(path);
            let wt_meta = fs::symlink_metadata(&abs_path).ok();

            // Check if any ancestor directory component of `path` currently exists on disk as an untracked regular file/symlink
            if t_val.is_some() {
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
                            if !head_tree_map.contains_key(&anc_key)
                                && index.get(&anc_key, 0).is_none()
                            {
                                ancestor_conflict = Some(anc_key);
                                break;
                            }
                        }
                    }
                    curr_rel = anc.parent();
                }
                if let Some(anc_key) = ancestor_conflict {
                    if !untracked_overwrite_conflicts.contains(&anc_key) {
                        untracked_overwrite_conflicts.push(anc_key);
                    }
                    continue;
                }
            }

            if h_val.is_none() && i_entry.is_none() {
                // Path was neither in H nor in I, and T is adding it
                if let Some(ref meta) = wt_meta {
                    if meta.file_type().is_dir() {
                        // Directory on disk: only conflicts if it contains at least one untracked file
                        if dir_contains_untracked_file(&abs_path, path, &head_tree_map, &index) {
                            untracked_overwrite_conflicts.push(path.clone());
                            continue;
                        }
                    } else {
                        untracked_overwrite_conflicts.push(path.clone());
                        continue;
                    }
                }
            } else if let Some(ie) = i_entry {
                // Path is currently tracked: check if working tree has unstaged modifications relative to `ie`
                if let Some(ref meta) = wt_meta {
                    if meta.file_type().is_dir() {
                        if dir_contains_untracked_file(&abs_path, path, &head_tree_map, &index) {
                            untracked_overwrite_conflicts.push(path.clone());
                        } else {
                            dirty_tracked_conflicts.push(path.clone());
                        }
                        continue;
                    }
                    let wt_mode = mode_from_metadata(meta);
                    let wt_bytes = read_worktree_entry(&abs_path, meta)?;
                    let wt_oid = ObjectId::hash_object(ObjectKind::Blob.as_str(), &wt_bytes);
                    if (wt_mode, wt_oid) != (ie.file_mode(), ie.oid)
                        && Some((wt_mode, wt_oid)) != t_val
                    {
                        dirty_tracked_conflicts.push(path.clone());
                        continue;
                    }
                } else if t_val.is_some() {
                    // Tracked file was deleted in working tree while T modifies it
                    dirty_tracked_conflicts.push(path.clone());
                    continue;
                }
            }
        }

        if !dirty_tracked_conflicts.is_empty() {
            return Err(GirError::InvalidArgument(format!(
                "Your local changes to the following files would be overwritten by checkout:\n\t{}\nPlease commit your changes or stash them before you switch branches.\nAborting",
                dirty_tracked_conflicts.join("\n\t")
            )));
        }

        if !untracked_overwrite_conflicts.is_empty() {
            return Err(GirError::InvalidArgument(format!(
                "The following untracked working tree files would be overwritten by checkout:\n\t{}\nPlease move or remove them before you switch branches.\nAborting",
                untracked_overwrite_conflicts.join("\n\t")
            )));
        }
    }

    // Phase 2: Capture pre-operation safety snapshot before mutating disk or index
    let _ = SafetySnapshotEngine::capture(repo, snapshot_label);

    // Step 2a: Delete files that are removed in T (or all non-T tracked files if `force`)
    for path in &all_paths {
        let h_val = head_tree_map.get(path).copied();
        let t_val = target_tree_map.get(path).copied();

        if !force && h_val == t_val {
            continue;
        }

        if t_val.is_none() {
            let abs_path = worktree.join(path);
            if fs::symlink_metadata(&abs_path).is_ok() {
                let _ = fs::remove_file(&abs_path);
                prune_empty_parent_dirs(&abs_path, &worktree);
            }
            index.remove_path(path);
        }
    }

    // Step 2b: Materialize files added or modified in T (or all files in T if `force`)
    for (path, &(t_mode, t_oid)) in &target_tree_map {
        let h_val = head_tree_map.get(path).copied();
        if !force && h_val == Some((t_mode, t_oid)) {
            continue;
        }

        let abs_path = worktree.join(path);
        materialize_blob_to_worktree(repo.odb(), &worktree, &abs_path, t_mode, &t_oid)?;
        let meta = fs::symlink_metadata(&abs_path)?;
        index.add_entry(IndexEntry::from_metadata(path, t_oid, 0, &meta));
    }

    let _ = write_index_to_tree(&mut index, repo.odb());
    index.write(repo.git_dir())?;
    Ok(())
}

/// Writes a single `(mode, oid)` entry from `odb` to `abs_path` in `worktree`.
pub fn materialize_blob_to_worktree(
    odb: &crate::storage::ObjectDatabase,
    worktree: &Path,
    abs_path: &Path,
    mode: FileMode,
    oid: &ObjectId,
) -> Result<(), GirError> {
    // Ensure no ancestor of `abs_path` is a non-directory file
    if let Some(parent) = abs_path.parent() {
        let mut curr = parent;
        while curr != worktree && curr.starts_with(worktree) {
            if let Ok(meta) = fs::symlink_metadata(curr) {
                if !meta.file_type().is_dir() {
                    let _ = fs::remove_file(curr);
                }
            }
            let Some(p) = curr.parent() else {
                break;
            };
            curr = p;
        }
        fs::create_dir_all(parent)?;
    }

    // If `abs_path` currently exists as a directory or symlink, remove it first
    if let Ok(meta) = fs::symlink_metadata(abs_path) {
        if meta.file_type().is_dir() {
            let _ = fs::remove_dir_all(abs_path);
        } else if meta.file_type().is_symlink() || mode == FileMode::Symlink {
            let _ = fs::remove_file(abs_path);
        }
    }

    let raw = odb.read_raw(oid)?;
    if mode == FileMode::Symlink {
        let link_target = String::from_utf8_lossy(&raw.data).into_owned();
        std::os::unix::fs::symlink(link_target, abs_path)?;
    } else {
        let mut lock = LockFile::acquire(abs_path)?;
        lock.write_all(&raw.data)?;
        lock.commit()?;
        let perm = if mode == FileMode::Executable {
            0o755
        } else {
            0o644
        };
        let _ = fs::set_permissions(abs_path, fs::Permissions::from_mode(perm));
    }

    Ok(())
}

/// Removes empty parent directories of `removed_file` up to (but not including) `worktree`.
pub fn prune_empty_parent_dirs(removed_file: &Path, worktree: &Path) {
    let mut curr = removed_file.parent();
    while let Some(dir) = curr {
        if dir == worktree || !dir.starts_with(worktree) {
            break;
        }
        if fs::remove_dir(dir).is_err() {
            break;
        }
        curr = dir.parent();
    }
}

pub(crate) fn dir_contains_untracked_file(
    abs_dir: &Path,
    rel_dir: &str,
    head_tree_map: &BTreeMap<String, (FileMode, ObjectId)>,
    index: &GitIndex,
) -> bool {
    let Ok(entries) = fs::read_dir(abs_dir) else {
        return false;
    };
    for entry in entries.flatten() {
        let fname = entry.file_name();
        let Some(name_str) = fname.to_str() else {
            continue;
        };
        let rel_path = format!("{rel_dir}/{name_str}");
        let abs_path = entry.path();
        let Ok(meta) = fs::symlink_metadata(&abs_path) else {
            continue;
        };
        if meta.file_type().is_dir() {
            if dir_contains_untracked_file(&abs_path, &rel_path, head_tree_map, index) {
                return true;
            }
        } else if !head_tree_map.contains_key(&rel_path) && index.get(&rel_path, 0).is_none() {
            return true;
        }
    }
    false
}

pub(crate) fn fallback_sig(repo: &Repository) -> Signature {
    repo.resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"))
}

pub(crate) fn read_commit_tree_map(
    repo: &Repository,
    spec: &str,
) -> Result<(ObjectId, BTreeMap<String, (FileMode, ObjectId)>), GirError> {
    let oid = parse_revision(repo, spec)?;
    match repo.odb().read_parsed(&oid)? {
        ParsedObject::Commit(c) => Ok((c.tree, flatten_tree(repo.odb(), &c.tree)?)),
        ParsedObject::Tree(_) => Ok((oid, flatten_tree(repo.odb(), &oid)?)),
        ParsedObject::Tag(t) => {
            let (_, c) = peel_to_commit_object(repo.odb(), &t.object)?;
            Ok((c.tree, flatten_tree(repo.odb(), &c.tree)?))
        }
        other => Err(GirError::UnexpectedObjectType {
            expected: "tree-ish".to_string(),
            found: other.kind().to_string(),
        }),
    }
}
