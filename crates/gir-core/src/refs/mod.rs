pub mod packed;
pub mod transaction;

use std::collections::BTreeMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::Signature;
use crate::reflog::append_reflog;
use crate::storage::atomic::LockFile;

pub use packed::{PackedRefEntry, PackedRefs};
pub use transaction::ExpectedOldTarget;

/// Target of a Git reference on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefTarget {
    /// Direct 40-char hexadecimal `ObjectId`.
    Direct(ObjectId),
    /// Symbolic pointer (`ref: refs/heads/main`).
    Symbolic(String),
}

/// Manages loose references (`.git/refs/**`, `.git/HEAD`) and `.git/packed-refs`.
#[derive(Debug, Clone)]
pub struct RefStore {
    git_dir: PathBuf,
}

impl RefStore {
    pub fn new(git_dir: impl Into<PathBuf>) -> Self {
        Self {
            git_dir: git_dir.into(),
        }
    }

    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    /// Reads the immediate (non-dereferenced) value of `ref_name` from loose storage or `packed-refs`.
    pub fn read_raw(&self, ref_name: &str) -> Result<Option<RefTarget>, GirError> {
        let loose_path = self.git_dir.join(ref_name);
        if loose_path.is_file() {
            let content = fs::read_to_string(&loose_path)?;
            let trimmed = content.trim();
            if let Some(sym) = trimmed.strip_prefix("ref:") {
                return Ok(Some(RefTarget::Symbolic(sym.trim().to_string())));
            }
            return Ok(Some(RefTarget::Direct(ObjectId::from_hex(trimmed)?)));
        }

        let packed = PackedRefs::load(&self.git_dir)?;
        if let Some(entry) = packed.get(ref_name) {
            return Ok(Some(RefTarget::Direct(entry.oid)));
        }

        Ok(None)
    }

    /// Reads the symbolic target of `ref_name` (e.g. `HEAD` -> `refs/heads/main`),
    /// returning `GirError::NotASymbolicRef` if `ref_name` is a direct OID or missing.
    pub fn read_symbolic_ref(&self, ref_name: &str) -> Result<String, GirError> {
        match self.read_raw(ref_name)? {
            Some(RefTarget::Symbolic(target)) => Ok(target),
            _ => Err(GirError::NotASymbolicRef(ref_name.to_string())),
        }
    }

    /// Atomically updates `ref_name` to be a symbolic reference (`ref: <target_ref>`).
    pub fn set_symbolic_ref(
        &self,
        ref_name: &str,
        target_ref: &str,
        sig: Option<&Signature>,
        reflog_msg: Option<&str>,
    ) -> Result<(), GirError> {
        let old_oid = self.resolve_to_oid(ref_name).unwrap_or(ObjectId::ZERO);
        let ref_path = self.git_dir.join(ref_name);
        let mut lock = LockFile::acquire(&ref_path)?;
        writeln!(lock, "ref: {target_ref}")?;
        lock.commit()?;

        if let (Some(sig), Some(msg)) = (sig, reflog_msg) {
            if ! msg.is_empty() {
                let new_oid = self.resolve_to_oid(target_ref).unwrap_or(ObjectId::ZERO);
                if !old_oid.is_zero() || !new_oid.is_zero() {
                    append_reflog(&self.git_dir, ref_name, old_oid, new_oid, sig, msg)?;
                }
            }
        }

        Ok(())
    }

    /// Follows symbolic references (up to 10 hops) until a concrete `ObjectId` is reached.
    pub fn resolve_to_oid(&self, ref_name: &str) -> Result<ObjectId, GirError> {
        let mut current = ref_name.to_string();
        for _ in 0..10 {
            match self.read_raw(&current)? {
                Some(RefTarget::Direct(oid)) => return Ok(oid),
                Some(RefTarget::Symbolic(next)) => {
                    current = next;
                }
                None => break,
            }
        }
        Err(GirError::RefNotFound(ref_name.to_string()))
    }

    /// Follows symbolic references to return the terminal reference name (e.g. `HEAD` -> `refs/heads/main`),
    /// even if the terminal branch does not yet have its first commit (unborn branch).
    pub fn resolve_terminal_ref_name(&self, ref_name: &str) -> Result<String, GirError> {
        let mut current = ref_name.to_string();
        for _ in 0..10 {
            let loose_path = self.git_dir.join(&current);
            if loose_path.is_file() {
                let content = fs::read_to_string(&loose_path)?;
                if let Some(sym) = content.trim().strip_prefix("ref:") {
                    current = sym.trim().to_string();
                    continue;
                }
            }
            return Ok(current);
        }
        Ok(current)
    }

    /// Resolves a short or qualified reference name using Git's 6-level DWIM precedence rules:
    /// 1. `<name>` (if root pseudo-ref like `HEAD` or already `refs/...`)
    /// 2. `refs/<name>`
    /// 3. `refs/tags/<name>`
    /// 4. `refs/heads/<name>`
    /// 5. `refs/remotes/<name>`
    /// 6. `refs/remotes/<name>/HEAD`
    pub fn resolve_dwim(&self, name: &str) -> Result<(String, ObjectId), GirError> {
        let normalized = if name == "@" { "HEAD" } else { name };

        let candidates = [
            normalized.to_string(),
            format!("refs/{normalized}"),
            format!("refs/tags/{normalized}"),
            format!("refs/heads/{normalized}"),
            format!("refs/remotes/{normalized}"),
            format!("refs/remotes/{normalized}/HEAD"),
        ];

        for cand in &candidates {
            if !is_allowed_first_level_ref(cand) {
                continue;
            }
            if let Ok(oid) = self.resolve_to_oid(cand) {
                return Ok((cand.clone(), oid));
            }
        }

        Err(GirError::RefNotFound(name.to_string()))
    }

    /// Atomically updates `ref_name` to `new_oid` with compare-and-swap (`expected_old`) verification
    /// and appends the transition to `.git/logs/<ref_name>` (and `.git/logs/HEAD` if updating the active branch).
    pub fn update_ref(
        &self,
        ref_name: &str,
        new_oid: ObjectId,
        expected_old: Option<ObjectId>,
        no_deref: bool,
        sig: &Signature,
        reflog_msg: &str,
    ) -> Result<ObjectId, GirError> {
        let target_ref = if no_deref {
            ref_name.to_string()
        } else {
            self.resolve_terminal_ref_name(ref_name)?
        };

        let ref_path = self.git_dir.join(&target_ref);
        let mut lock = LockFile::acquire(&ref_path)?;

        let actual_old = self.resolve_to_oid(&target_ref).unwrap_or(ObjectId::ZERO);
        let cas = ExpectedOldTarget::from_optional_oid(expected_old);
        match cas {
            ExpectedOldTarget::Any => {}
            ExpectedOldTarget::MustNotExist => {
                if !actual_old.is_zero() {
                    return Err(GirError::RefCasMismatch {
                        reference: target_ref,
                        expected: ObjectId::ZERO.to_hex(),
                        actual: actual_old.to_hex(),
                    });
                }
            }
            ExpectedOldTarget::Exact(exp) => {
                if actual_old != exp {
                    return Err(GirError::RefCasMismatch {
                        reference: target_ref,
                        expected: exp.to_hex(),
                        actual: actual_old.to_hex(),
                    });
                }
            }
        }

        writeln!(lock, "{new_oid}")?;
        lock.commit()?;

        // Record in `.git/logs/<target_ref>`
        append_reflog(&self.git_dir, &target_ref, actual_old, new_oid, sig, reflog_msg)?;

        // Also record in `.git/logs/HEAD` if we updated the branch pointed to by `HEAD`
        if target_ref != "HEAD" {
            if let Ok(head_target) = self.read_symbolic_ref("HEAD") {
                if head_target == target_ref {
                    append_reflog(&self.git_dir, "HEAD", actual_old, new_oid, sig, reflog_msg)?;
                }
            }
        }

        Ok(actual_old)
    }

    /// Deletes a reference from both `.git/<ref_name>` and `.git/packed-refs` under lock,
    /// verifying `expected_old` if provided.
    pub fn delete_ref(
        &self,
        ref_name: &str,
        expected_old: Option<ObjectId>,
    ) -> Result<(), GirError> {
        let ref_path = self.git_dir.join(ref_name);
        let _lock = LockFile::acquire(&ref_path)?;

        let actual_old = self.resolve_to_oid(ref_name).unwrap_or(ObjectId::ZERO);
        if actual_old.is_zero() {
            return Err(GirError::RefNotFound(ref_name.to_string()));
        }

        if let Some(exp) = expected_old {
            if actual_old != exp {
                return Err(GirError::RefCasMismatch {
                    reference: ref_name.to_string(),
                    expected: exp.to_hex(),
                    actual: actual_old.to_hex(),
                });
            }
        }

        if ref_path.is_file() {
            fs::remove_file(&ref_path)?;
        }

        let log_path = self.git_dir.join("logs").join(ref_name);
        if log_path.is_file() {
            let _ = fs::remove_file(&log_path);
        }

        let mut packed = PackedRefs::load(&self.git_dir)?;
        if packed.remove(ref_name).is_some() {
            packed.save(&self.git_dir)?;
        }

        Ok(())
    }

    /// Lists all references (`HEAD` and `refs/**` from loose and `packed-refs`) in sorted order.
    pub fn list_all(&self) -> Result<Vec<(String, ObjectId)>, GirError> {
        let mut map = BTreeMap::new();

        let packed = PackedRefs::load(&self.git_dir)?;
        for (name, entry) in packed.entries() {
            map.insert(name.clone(), entry.oid);
        }

        let refs_dir = self.git_dir.join("refs");
        if refs_dir.is_dir() {
            self.collect_loose_into(&refs_dir, &mut map)?;
        }

        if let Ok(head_oid) = self.resolve_to_oid("HEAD") {
            map.insert("HEAD".to_string(), head_oid);
        }

        Ok(map.into_iter().collect())
    }

    fn collect_loose_into(
        &self,
        dir: &Path,
        map: &mut BTreeMap<String, ObjectId>,
    ) -> Result<(), GirError> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                self.collect_loose_into(&path, map)?;
            } else if path.is_file() {
                let Some(rel) = path.strip_prefix(&self.git_dir).ok() else {
                    continue;
                };
                let rel_str = rel.to_string_lossy().replace('\\', "/");
                if rel_str.ends_with(".lock") {
                    continue;
                }
                if let Ok(oid) = self.resolve_to_oid(&rel_str) {
                    map.insert(rel_str, oid);
                }
            }
        }
        Ok(())
    }
}

fn is_allowed_first_level_ref(cand: &str) -> bool {
    cand.starts_with("refs/")
        || matches!(
            cand,
            "HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "MERGE_HEAD" | "CHERRY_PICK_HEAD" | "REVERT_HEAD"
        )
}
