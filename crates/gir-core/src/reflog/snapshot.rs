use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::{Commit, Signature};
use crate::object::tree::{FileMode, Tree, TreeEntry};
use crate::object::{ObjectKind, ParsedObject};
use crate::reflog::{append_reflog, read_reflog, ReflogEntry};
use crate::repository::Repository;
use crate::storage::atomic::LockFile;

pub const SNAPSHOT_REF: &str = "refs/gir/snapshots/latest";

/// Metadata describing a captured pre-operation safety snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotRecord {
    pub snapshot_oid: ObjectId,
    pub worktree_tree_oid: ObjectId,
    pub head_raw: String,
    pub head_oid: ObjectId,
    pub operation: String,
}

/// Summary returned after executing `gir undo`.
#[derive(Debug, Clone)]
pub struct UndoReport {
    pub restored_snapshot: ObjectId,
    pub operation: String,
    pub restored_files: usize,
    pub head_oid: ObjectId,
}

/// Zero-data-loss pre-operation safety snapshot engine (`refs/gir/snapshots/latest`).
pub struct SafetySnapshotEngine;

impl SafetySnapshotEngine {
    /// Captures the entire working tree, `.git/index`, and `HEAD` state into standard Git
    /// `Commit`/`Tree`/`Blob` objects under `refs/gir/snapshots/latest`.
    pub fn capture(repo: &Repository, operation: &str) -> Result<SnapshotRecord, GirError> {
        let Some(worktree) = repo.worktree() else {
            return Err(GirError::InvalidArgument(
                "cannot capture working-tree snapshot in a bare repository".to_string(),
            ));
        };

        let sig = repo
            .resolve_committer_signature()
            .unwrap_or_else(|_| Signature::now("gir-snapshot", "gir@localhost"));

        // 1. Snapshot the working directory tree (excluding `.git`)
        let worktree_tree_oid = write_directory_to_odb(repo, worktree)?;

        // 2. Capture current HEAD raw string and resolved commit OID (if any)
        let head_path = repo.git_dir().join("HEAD");
        let head_raw = if head_path.is_file() {
            fs::read_to_string(&head_path)?.trim().to_string()
        } else {
            "ref: refs/heads/main".to_string()
        };
        let head_oid = repo.refs().resolve_to_oid("HEAD").unwrap_or(ObjectId::ZERO);

        let mut parents = Vec::new();
        if !head_oid.is_zero() {
            parents.push(head_oid);
        }

        // 3. If `.git/index` exists, store it inside a valid helper commit as parent[1]
        let index_path = repo.git_dir().join("index");
        if index_path.is_file() {
            let index_bytes = fs::read(&index_path)?;
            let index_blob_oid = repo.odb().write_raw(ObjectKind::Blob, &index_bytes)?;
            let index_tree = Tree::new(vec![TreeEntry::new(
                FileMode::Regular,
                "index",
                index_blob_oid,
            )]);
            let index_tree_oid = repo.odb().write_object(&index_tree.to_raw())?;
            let index_commit = Commit {
                tree: index_tree_oid,
                parents: if head_oid.is_zero() {
                    Vec::new()
                } else {
                    vec![head_oid]
                },
                author: sig.clone(),
                committer: sig.clone(),
                message: "gir-snapshot-index\n".to_string(),
            };
            let index_commit_oid = repo.odb().write_object(&index_commit.to_raw())?;
            parents.push(index_commit_oid);
        }

        // 4. Write the top-level snapshot Commit object
        let message = format!(
            "gir-snapshot: {operation}\nHEAD-raw: {head_raw}\nHEAD-oid: {head_oid}\n"
        );
        let snapshot_commit = Commit {
            tree: worktree_tree_oid,
            parents,
            author: sig.clone(),
            committer: sig.clone(),
            message,
        };
        let snapshot_oid = repo.odb().write_object(&snapshot_commit.to_raw())?;

        // 5. Update `refs/gir/snapshots/latest` and append to its reflog
        let old_snap = repo
            .refs()
            .resolve_to_oid(SNAPSHOT_REF)
            .unwrap_or(ObjectId::ZERO);
        let snap_ref_path = repo.git_dir().join(SNAPSHOT_REF);
        let mut lock = LockFile::acquire(&snap_ref_path)?;
        writeln!(lock, "{snapshot_oid}")?;
        lock.commit()?;

        append_reflog(
            repo.git_dir(),
            SNAPSHOT_REF,
            old_snap,
            snapshot_oid,
            &sig,
            &format!("snapshot: {operation}"),
        )?;

        Ok(SnapshotRecord {
            snapshot_oid,
            worktree_tree_oid,
            head_raw,
            head_oid,
            operation: operation.to_string(),
        })
    }

    /// Lists all recorded safety snapshots (most recent first).
    pub fn list(repo: &Repository) -> Result<Vec<ReflogEntry>, GirError> {
        let mut entries = read_reflog(repo.git_dir(), SNAPSHOT_REF)?;
        entries.reverse();
        Ok(entries)
    }

    /// Restores the repository working directory, `.git/index`, `HEAD`, and branch reference
    /// from the specified snapshot (or the most recent snapshot if `target` is `None`).
    pub fn restore(repo: &Repository, target: Option<&str>) -> Result<UndoReport, GirError> {
        let Some(worktree) = repo.worktree() else {
            return Err(GirError::InvalidArgument(
                "cannot run `gir undo` in a bare repository".to_string(),
            ));
        };

        let snapshot_oid = match target {
            Some(spec) => {
                if let Some(idx_str) = spec.strip_prefix("snapshot@{") {
                    let idx_str = idx_str.trim_end_matches('}');
                    let idx: usize = idx_str
                        .parse()
                        .map_err(|_| GirError::InvalidRevision(spec.to_string()))?;
                    crate::reflog::resolve_reflog_at(repo.git_dir(), SNAPSHOT_REF, idx)?
                } else {
                    repo.odb().resolve_prefix(spec)?
                }
            }
            None => repo.refs().resolve_to_oid(SNAPSHOT_REF).map_err(|_| {
                GirError::InvalidArgument(
                    "no safety snapshots recorded yet in refs/gir/snapshots/latest".to_string(),
                )
            })?,
        };

        let ParsedObject::Commit(snap_commit) = repo.odb().read_parsed(&snapshot_oid)? else {
            return Err(GirError::InvalidArgument(format!(
                "object {snapshot_oid} is not a snapshot commit"
            )));
        };

        let mut operation = "snapshot".to_string();
        let mut head_raw = None;
        let mut head_oid = ObjectId::ZERO;

        for line in snap_commit.message.lines() {
            if let Some(op) = line.strip_prefix("gir-snapshot: ") {
                operation = op.trim().to_string();
            } else if let Some(hr) = line.strip_prefix("HEAD-raw: ") {
                head_raw = Some(hr.trim().to_string());
            } else if let Some(ho) = line.strip_prefix("HEAD-oid: ") {
                if let Ok(id) = ObjectId::from_hex(ho.trim()) {
                    head_oid = id;
                }
            }
        }

        // 1. Restore working directory to match `snap_commit.tree`
        let restored_files = restore_directory_from_tree(repo, worktree, &snap_commit.tree)?;

        // 2. Restore `.git/index` if stored in parent[1] (or remove `.git/index` if absent at snapshot time)
        let mut restored_index = false;
        for parent_oid in &snap_commit.parents {
            if *parent_oid == head_oid {
                continue;
            }
            if let Ok(ParsedObject::Commit(idx_commit)) = repo.odb().read_parsed(parent_oid) {
                if idx_commit.message.starts_with("gir-snapshot-index") {
                    if let Ok(ParsedObject::Tree(idx_tree)) =
                        repo.odb().read_parsed(&idx_commit.tree)
                    {
                        if let Some(entry) = idx_tree.entries.iter().find(|e| e.name == "index") {
                            let raw_blob = repo.odb().read_raw(&entry.oid)?;
                            let index_path = repo.git_dir().join("index");
                            let mut lock = LockFile::acquire(&index_path)?;
                            lock.write_all(&raw_blob.data)?;
                            lock.commit()?;
                            restored_index = true;
                        }
                    }
                }
            }
        }
        if !restored_index {
            let index_path = repo.git_dir().join("index");
            if index_path.is_file() {
                let _ = fs::remove_file(&index_path);
            }
        }

        // 3. Restore `HEAD` and target branch reference
        let sig = repo
            .resolve_committer_signature()
            .unwrap_or_else(|_| Signature::now("gir-undo", "gir@localhost"));

        if let Some(ref raw_head) = head_raw {
            if let Some(sym_target) = raw_head.strip_prefix("ref:") {
                let sym_target = sym_target.trim();
                repo.refs().set_symbolic_ref("HEAD", sym_target, None, None)?;
                if !head_oid.is_zero() {
                    repo.refs().update_ref(
                        sym_target,
                        head_oid,
                        None,
                        true,
                        &sig,
                        &format!("undo: restored snapshot {}", &snapshot_oid.to_hex()[..7]),
                    )?;
                }
            } else if !head_oid.is_zero() {
                repo.refs().update_ref(
                    "HEAD",
                    head_oid,
                    None,
                    true,
                    &sig,
                    &format!("undo: restored snapshot {}", &snapshot_oid.to_hex()[..7]),
                )?;
            }
        }

        Ok(UndoReport {
            restored_snapshot: snapshot_oid,
            operation,
            restored_files,
            head_oid,
        })
    }
}

fn write_directory_to_odb(repo: &Repository, dir: &Path) -> Result<ObjectId, GirError> {
    let mut entries = Vec::new();

    for dir_entry in fs::read_dir(dir)? {
        let dir_entry = dir_entry?;
        let file_name = dir_entry.file_name();
        let Some(name_str) = file_name.to_str() else {
            continue;
        };
        if name_str == ".git" {
            continue;
        }

        let path = dir_entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let file_type = meta.file_type();
        if file_type.is_dir() {
            let sub_oid = write_directory_to_odb(repo, &path)?;
            entries.push(TreeEntry::new(
                FileMode::Directory,
                name_str.to_string(),
                sub_oid,
            ));
        } else if file_type.is_symlink() {
            let link_target = fs::read_link(&path)?;
            let target_bytes = link_target.to_string_lossy().into_owned().into_bytes();
            let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &target_bytes)?;
            entries.push(TreeEntry::new(
                FileMode::Symlink,
                name_str.to_string(),
                blob_oid,
            ));
        } else if file_type.is_file() {
            let data = fs::read(&path)?;
            let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &data)?;
            let is_exec = (meta.permissions().mode() & 0o111) != 0;
            let mode = if is_exec {
                FileMode::Executable
            } else {
                FileMode::Regular
            };
            entries.push(TreeEntry::new(mode, name_str.to_string(), blob_oid));
        }
    }

    let tree = Tree::new(entries);
    repo.odb().write_object(&tree.to_raw())
}

fn restore_directory_from_tree(
    repo: &Repository,
    dir: &Path,
    tree_oid: &ObjectId,
) -> Result<usize, GirError> {
    fs::create_dir_all(dir)?;
    let ParsedObject::Tree(tree) = repo.odb().read_parsed(tree_oid)? else {
        return Err(GirError::UnexpectedObjectType {
            expected: "tree".to_string(),
            found: "non-tree".to_string(),
        });
    };

    let expected_names: HashSet<&str> = tree.entries.iter().map(|e| e.name.as_str()).collect();

    // Remove working-tree files/directories in `dir` that were not present in the snapshot tree
    for existing in fs::read_dir(dir)? {
        let existing = existing?;
        let fname = existing.file_name();
        let Some(fname_str) = fname.to_str() else {
            continue;
        };
        if fname_str == ".git" {
            continue;
        }
        if !expected_names.contains(fname_str) {
            let p = existing.path();
            let meta = fs::symlink_metadata(&p)?;
            if meta.file_type().is_dir() {
                fs::remove_dir_all(&p)?;
            } else {
                fs::remove_file(&p)?;
            }
        }
    }

    let mut restored_count = 0;
    for entry in &tree.entries {
        let entry_path: PathBuf = dir.join(&entry.name);
        if entry.mode == FileMode::Directory {
            if let Ok(meta) = fs::symlink_metadata(&entry_path) {
                if !meta.file_type().is_dir() {
                    fs::remove_file(&entry_path)?;
                }
            }
            restored_count += restore_directory_from_tree(repo, &entry_path, &entry.oid)?;
        } else if entry.mode == FileMode::Symlink {
            if let Ok(meta) = fs::symlink_metadata(&entry_path) {
                if meta.file_type().is_dir() {
                    fs::remove_dir_all(&entry_path)?;
                } else {
                    fs::remove_file(&entry_path)?;
                }
            }
            let blob_raw = repo.odb().read_raw(&entry.oid)?;
            let target_str = String::from_utf8_lossy(&blob_raw.data);
            #[cfg(unix)]
            std::os::unix::fs::symlink(target_str.as_ref(), &entry_path)?;
            restored_count += 1;
        } else {
            if let Ok(meta) = fs::symlink_metadata(&entry_path) {
                if meta.file_type().is_dir() {
                    fs::remove_dir_all(&entry_path)?;
                } else if meta.file_type().is_symlink() {
                    fs::remove_file(&entry_path)?;
                }
            }
            let blob_raw = repo.odb().read_raw(&entry.oid)?;
            fs::write(&entry_path, &blob_raw.data)?;
            let mut perms = fs::metadata(&entry_path)?.permissions();
            if entry.mode == FileMode::Executable {
                perms.set_mode(0o755);
            } else {
                perms.set_mode(0o644);
            }
            fs::set_permissions(&entry_path, perms)?;
            restored_count += 1;
        }
    }

    Ok(restored_count)
}
