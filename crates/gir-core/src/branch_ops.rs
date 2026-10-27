use std::fs;
use std::io::Write;

use crate::error::GirError;
use crate::graph::{is_ancestor, peel_to_commit_object};
use crate::hash::ObjectId;
use crate::object::commit::Signature;
use crate::reflog::append_reflog;
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::storage::atomic::LockFile;

/// Metadata for a local or remote-tracking branch returned by `list_branches`.
#[derive(Debug, Clone)]
pub struct BranchEntry {
    pub name: String,
    pub full_ref: String,
    pub oid: ObjectId,
    pub is_current: bool,
    pub is_remote: bool,
    pub subject: String,
}

/// Validates that `name` is a legal Git branch or tag short name (`git check-ref-format`).
pub fn validate_ref_name(name: &str) -> Result<(), GirError> {
    if name.is_empty()
        || name == "@"
        || name.starts_with('/')
        || name.ends_with('/')
        || name.contains("..")
        || name.contains("@{")
        || name.contains("//")
        || name
            .split('/')
            .any(|part| part.is_empty() || part.starts_with('.') || part.ends_with('.') || part.ends_with(".lock"))
        || name.chars().any(|c| {
            c.is_ascii_control()
                || c == ' '
                || matches!(c, '~' | '^' | ':' | '?' | '*' | '[' | '\\')
        })
    {
        return Err(GirError::InvalidArgument(format!(
            "'{name}' is not a valid branch or tag name"
        )));
    }
    Ok(())
}

/// Lists local and/or remote-tracking branches sorted lexicographically.
pub fn list_branches(
    repo: &Repository,
    include_remotes: bool,
    remotes_only: bool,
) -> Result<Vec<BranchEntry>, GirError> {
    let refs = repo.refs();
    let current_ref = match refs.read_raw("HEAD")? {
        Some(RefTarget::Symbolic(sym)) => Some(sym),
        _ => None,
    };

    let all_refs = refs.list_all()?;
    let mut entries = Vec::new();

    for (ref_name, oid) in all_refs {
        if let Some(short) = ref_name.strip_prefix("refs/heads/") {
            if remotes_only {
                continue;
            }
            let subject = peel_to_commit_object(repo.odb(), &oid)
                .map(|(_, c)| c.subject().to_string())
                .unwrap_or_default();
            let is_current = current_ref.as_deref() == Some(ref_name.as_str());
            entries.push(BranchEntry {
                name: short.to_string(),
                full_ref: ref_name,
                oid,
                is_current,
                is_remote: false,
                subject,
            });
        } else if let Some(short) = ref_name.strip_prefix("refs/remotes/") {
            if !include_remotes && !remotes_only {
                continue;
            }
            let subject = peel_to_commit_object(repo.odb(), &oid)
                .map(|(_, c)| c.subject().to_string())
                .unwrap_or_default();
            let display_name = if remotes_only {
                short.to_string()
            } else {
                format!("remotes/{short}")
            };
            entries.push(BranchEntry {
                name: display_name,
                full_ref: ref_name,
                oid,
                is_current: false,
                is_remote: true,
                subject,
            });
        }
    }

    Ok(entries)
}

/// Creates a new local branch `refs/heads/<name>` pointing at `start_point` (defaulting to `HEAD`).
pub fn create_branch(
    repo: &Repository,
    name: &str,
    start_point: Option<&str>,
    force: bool,
) -> Result<ObjectId, GirError> {
    validate_ref_name(name)?;
    let full_ref = format!("refs/heads/{name}");

    if !force && repo.refs().resolve_to_oid(&full_ref).is_ok() {
        return Err(GirError::InvalidArgument(format!(
            "a branch named '{name}' already exists"
        )));
    }

    let start_spec = start_point.unwrap_or("HEAD");
    let raw_oid = parse_revision(repo, start_spec)?;
    let (commit_oid, _) = peel_to_commit_object(repo.odb(), &raw_oid)?;

    let sig = fallback_signature(repo);
    let expected_old = if force { None } else { Some(ObjectId::ZERO) };
    let msg = format!("branch: Created from {start_spec}");
    repo.refs()
        .update_ref(&full_ref, commit_oid, expected_old, true, &sig, &msg)?;

    Ok(commit_oid)
}

/// Deletes a local branch `refs/heads/<name>` (checking reachability from `HEAD` unless `force` is set).
pub fn delete_branch(repo: &Repository, name: &str, force: bool) -> Result<ObjectId, GirError> {
    let full_ref = format!("refs/heads/{name}");
    if let Ok(head_sym) = repo.refs().read_symbolic_ref("HEAD") {
        if head_sym == full_ref {
            return Err(GirError::InvalidArgument(format!(
                "Cannot delete branch '{name}' checked out at '{}'",
                repo.worktree()
                    .unwrap_or_else(|| repo.git_dir())
                    .display()
            )));
        }
    }

    let branch_oid = repo
        .refs()
        .resolve_to_oid(&full_ref)
        .map_err(|_| GirError::InvalidArgument(format!("branch '{name}' not found.")))?;

    if !force {
        let head_oid = repo.refs().resolve_to_oid("HEAD")?;
        if !is_ancestor(repo.odb(), &branch_oid, &head_oid)? {
            return Err(GirError::InvalidArgument(format!(
                "The branch '{name}' is not fully merged.\nIf you are sure you want to delete it, run 'gir branch -D {name}'."
            )));
        }
    }

    repo.refs().delete_ref(&full_ref, Some(branch_oid))?;

    // Remove `[branch "<name>"]` from `.git/config` if present
    let config_path = repo.git_dir().join("config");
    if let Ok(mut cfg) = repo.local_config() {
        if cfg.remove_section("branch", Some(name)).is_some() {
            let _ = cfg.save_to(&config_path);
        }
    }

    Ok(branch_oid)
}

/// Renames a local branch (`old_name` or current branch) to `new_name`,
/// handling both unborn branches and committed branches (with reflog and `.git/config` migration).
pub fn rename_branch(
    repo: &Repository,
    old_name_opt: Option<&str>,
    new_name: &str,
    force: bool,
) -> Result<(), GirError> {
    validate_ref_name(new_name)?;

    let head_sym = repo.refs().read_symbolic_ref("HEAD").ok();
    let old_name = match old_name_opt {
        Some(n) => n.to_string(),
        None => {
            let Some(ref sym) = head_sym else {
                return Err(GirError::InvalidArgument(
                    "cannot rename the current branch while not on any".to_string(),
                ));
            };
            sym.strip_prefix("refs/heads/")
                .unwrap_or(sym)
                .to_string()
        }
    };

    let old_ref = format!("refs/heads/{old_name}");
    let new_ref = format!("refs/heads/{new_name}");

    if old_name == new_name {
        if !force && repo.refs().resolve_to_oid(&old_ref).is_ok() {
            return Err(GirError::InvalidArgument(format!(
                "a branch named '{new_name}' already exists"
            )));
        }
        return Ok(());
    }

    if !force && repo.refs().resolve_to_oid(&new_ref).is_ok() {
        return Err(GirError::InvalidArgument(format!(
            "a branch named '{new_name}' already exists"
        )));
    }

    let is_head_branch = head_sym.as_deref() == Some(old_ref.as_str());

    // Case 1: Unborn branch (`HEAD` points to `refs/heads/<old_name>` which has no commits yet)
    if is_head_branch && repo.refs().resolve_to_oid(&old_ref).is_err() {
        if force && repo.refs().resolve_to_oid(&new_ref).is_ok() {
            let _ = repo.refs().delete_ref(&new_ref, None);
        }
        repo.refs()
            .set_symbolic_ref("HEAD", &new_ref, None, None)?;
        rename_branch_config_section(repo, &old_name, new_name);
        return Ok(());
    }

    // Case 2: Committed branch
    let oid = repo
        .refs()
        .resolve_to_oid(&old_ref)
        .map_err(|_| GirError::InvalidArgument(format!("refname {old_ref} not found")))?;

    if force && repo.refs().resolve_to_oid(&new_ref).is_ok() {
        let _ = repo.refs().delete_ref(&new_ref, None);
    }

    // Write `refs/heads/<new_name>` atomically
    let new_ref_path = repo.git_dir().join(&new_ref);
    {
        let mut lock = LockFile::acquire(&new_ref_path)?;
        writeln!(lock, "{oid}")?;
        lock.commit()?;
    }

    // Move `.git/logs/refs/heads/<old_name>` -> `.git/logs/refs/heads/<new_name>` and append reflog
    let old_log = repo.git_dir().join("logs").join(&old_ref);
    let new_log = repo.git_dir().join("logs").join(&new_ref);
    if old_log.is_file() {
        if let Some(parent) = new_log.parent() {
            let _ = fs::create_dir_all(parent);
        }
        let _ = fs::rename(&old_log, &new_log);
    }

    let sig = fallback_signature(repo);
    let reflog_msg = format!("Branch: renamed {old_ref} to {new_ref}");
    append_reflog(repo.git_dir(), &new_ref, oid, oid, &sig, &reflog_msg)?;

    // Delete old branch ref
    repo.refs().delete_ref(&old_ref, Some(oid))?;

    // If `old_ref` was checked out, point `HEAD` to `new_ref` and record in `HEAD` reflog
    if is_head_branch {
        repo.refs()
            .set_symbolic_ref("HEAD", &new_ref, None, None)?;
        append_reflog(repo.git_dir(), "HEAD", oid, oid, &sig, &reflog_msg)?;
    }

    rename_branch_config_section(repo, &old_name, new_name);
    Ok(())
}

fn rename_branch_config_section(repo: &Repository, old_name: &str, new_name: &str) {
    let config_path = repo.git_dir().join("config");
    if let Ok(mut cfg) = repo.local_config() {
        if let Some(kv) = cfg.remove_section("branch", Some(old_name)) {
            for (k, v) in kv {
                let _ = cfg.set(&format!("branch.{new_name}.{k}"), v);
            }
            let _ = cfg.save_to(&config_path);
        }
    }
}

fn fallback_signature(repo: &Repository) -> Signature {
    repo.resolve_committer_signature()
        .unwrap_or_else(|_| Signature::now("gir", "gir@localhost"))
}
