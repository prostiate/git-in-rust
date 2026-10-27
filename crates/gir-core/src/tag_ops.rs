use std::fs;
use std::io::Write;

use crate::branch_ops::validate_ref_name;
use crate::commit_ops::normalize_commit_message;
use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::tag::Tag;
use crate::refs::PackedRefs;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::storage::atomic::LockFile;

/// Lists all tags (`refs/tags/*`) in lexicographical order, optionally filtered by a glob/substring pattern.
pub fn list_tags(repo: &Repository, pattern: Option<&str>) -> Result<Vec<String>, GirError> {
    let all_refs = repo.refs().list_all()?;
    let mut tags = Vec::new();
    for (ref_name, _) in all_refs {
        if let Some(short) = ref_name.strip_prefix("refs/tags/") {
            if let Some(pat) = pattern {
                if !matches_tag_pattern(short, pat) {
                    continue;
                }
            }
            tags.push(short.to_string());
        }
    }
    tags.sort();
    Ok(tags)
}

fn matches_tag_pattern(tag: &str, pattern: &str) -> bool {
    if !pattern.contains('*') && !pattern.contains('?') {
        return tag.contains(pattern);
    }
    crate::ignore::pattern::glob_match(pattern, tag)
}

/// Creates a lightweight tag (`refs/tags/<name>`) pointing directly at `target` (defaulting to `HEAD`).
pub fn create_lightweight_tag(
    repo: &Repository,
    name: &str,
    target: Option<&str>,
    force: bool,
) -> Result<ObjectId, GirError> {
    validate_ref_name(name)?;
    let full_ref = format!("refs/tags/{name}");
    if !force && repo.refs().resolve_to_oid(&full_ref).is_ok() {
        return Err(GirError::InvalidArgument(format!(
            "tag '{name}' already exists"
        )));
    }

    let target_oid = parse_revision(repo, target.unwrap_or("HEAD"))?;
    write_tag_ref(repo, &full_ref, target_oid)?;
    Ok(target_oid)
}

/// Creates an annotated `Tag` object in `odb` and writes `refs/tags/<name>` pointing to the `Tag` object.
pub fn create_annotated_tag(
    repo: &Repository,
    name: &str,
    target: Option<&str>,
    message: &str,
    force: bool,
) -> Result<ObjectId, GirError> {
    validate_ref_name(name)?;
    let full_ref = format!("refs/tags/{name}");
    if !force && repo.refs().resolve_to_oid(&full_ref).is_ok() {
        return Err(GirError::InvalidArgument(format!(
            "tag '{name}' already exists"
        )));
    }

    let target_oid = parse_revision(repo, target.unwrap_or("HEAD"))?;
    let object_kind = repo.odb().read_raw(&target_oid)?.kind;
    let tagger = repo.resolve_committer_signature()?;
    let norm_msg = normalize_commit_message(message);

    let tag = Tag {
        object: target_oid,
        object_kind,
        tag_name: name.to_string(),
        tagger: Some(tagger),
        message: norm_msg,
    };

    let tag_oid = repo.odb().write_object(&tag.to_raw())?;
    write_tag_ref(repo, &full_ref, tag_oid)?;
    Ok(tag_oid)
}

/// Deletes a tag (`refs/tags/<name>`) and returns its previous `ObjectId`.
pub fn delete_tag(repo: &Repository, name: &str) -> Result<ObjectId, GirError> {
    let full_ref = format!("refs/tags/{name}");
    let old_oid = repo
        .refs()
        .resolve_to_oid(&full_ref)
        .map_err(|_| GirError::InvalidArgument(format!("tag '{name}' not found.")))?;

    let ref_path = repo.git_dir().join(&full_ref);
    let _lock = LockFile::acquire(&ref_path)?;
    if ref_path.is_file() {
        fs::remove_file(&ref_path)?;
    }

    let mut packed = PackedRefs::load(repo.git_dir())?;
    if packed.remove(&full_ref).is_some() {
        packed.save(repo.git_dir())?;
    }

    Ok(old_oid)
}

fn write_tag_ref(repo: &Repository, full_ref: &str, oid: ObjectId) -> Result<(), GirError> {
    let ref_path = repo.git_dir().join(full_ref);
    let mut lock = LockFile::acquire(&ref_path)?;
    writeln!(lock, "{oid}")?;
    lock.commit()
}
