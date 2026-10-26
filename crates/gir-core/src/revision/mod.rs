use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::ParsedObject;
use crate::reflog::{resolve_previous_checkout_branch, resolve_reflog_at};
use crate::repository::Repository;

/// Parsed revision specification (either a single object or a commit range `A..B` / `A...B`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RevisionSpec {
    Single(ObjectId),
    Range { from: ObjectId, to: ObjectId },
    SymmetricRange { left: ObjectId, right: ObjectId },
}

/// Parses a Git revision range (`A..B`, `A...B`, or single `<rev>`).
pub fn parse_revision_spec(repo: &Repository, spec: &str) -> Result<RevisionSpec, GirError> {
    let spec = spec.trim();
    if let Some((left_str, right_str)) = spec.split_once("...") {
        let left = parse_revision(
            repo,
            if left_str.is_empty() { "HEAD" } else { left_str },
        )?;
        let right = parse_revision(
            repo,
            if right_str.is_empty() { "HEAD" } else { right_str },
        )?;
        return Ok(RevisionSpec::SymmetricRange { left, right });
    }

    if let Some((from_str, to_str)) = spec.split_once("..") {
        let from = parse_revision(
            repo,
            if from_str.is_empty() { "HEAD" } else { from_str },
        )?;
        let to = parse_revision(repo, if to_str.is_empty() { "HEAD" } else { to_str })?;
        return Ok(RevisionSpec::Range { from, to });
    }

    Ok(RevisionSpec::Single(parse_revision(repo, spec)?))
}

/// Resolves a Git revision expression (`HEAD~2^2`, `main@{1}`, `v1.0^{tree}`, `HEAD:src/lib.rs`, short SHA, etc.)
/// into a concrete `ObjectId`.
pub fn parse_revision(repo: &Repository, spec: &str) -> Result<ObjectId, GirError> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Err(GirError::InvalidRevision(spec.to_string()));
    }

    let (base, mut rest) = split_base_and_suffixes(spec);

    // 1. Resolve base (handling leading `@{N}` reflog lookups if present)
    let mut current_oid = if rest.starts_with("@{") {
        let Some(close_pos) = rest.find('}') else {
            return Err(GirError::InvalidRevision(spec.to_string()));
        };
        let inner = &rest[2..close_pos];
        rest = &rest[close_pos + 1..];

        if let Some(neg_str) = inner.strip_prefix('-') {
            if !base.is_empty() && base != "@" && base != "HEAD" {
                return Err(GirError::InvalidRevision(spec.to_string()));
            }
            let nth: usize = neg_str
                .parse()
                .map_err(|_| GirError::InvalidRevision(spec.to_string()))?;
            let prev_target = resolve_previous_checkout_branch(repo.git_dir(), nth)?;
            resolve_base_identifier(repo, &prev_target)?
        } else {
            let index: usize = inner
                .parse()
                .map_err(|_| GirError::InvalidRevision(spec.to_string()))?;

            let ref_name = if base.is_empty() {
                repo.refs().read_symbolic_ref("HEAD")?
            } else if base == "HEAD" || base == "@" {
                "HEAD".to_string()
            } else if let Ok((full_ref, _)) = repo.refs().resolve_dwim(base) {
                full_ref
            } else {
                format!("refs/heads/{base}")
            };

            resolve_reflog_at(repo.git_dir(), &ref_name, index)?
        }
    } else {
        resolve_base_identifier(repo, base)?
    };

    // 2. Apply chained suffix operators (`~N`, `^N`, `^{type}`, `:<path>`)
    while !rest.is_empty() {
        if let Some(after_tilde) = rest.strip_prefix('~') {
            let (digits, remaining) = take_digits(after_tilde);
            let steps: usize = if digits.is_empty() {
                1
            } else {
                digits
                    .parse()
                    .map_err(|_| GirError::InvalidRevision(spec.to_string()))?
            };
            rest = remaining;

            for _ in 0..steps {
                let (commit_oid, commit) = peel_to_commit(repo, &current_oid, spec)?;
                let _ = commit_oid;
                let Some(&first_parent) = commit.parents.first() else {
                    return Err(GirError::InvalidRevision(spec.to_string()));
                };
                current_oid = first_parent;
            }
        } else if let Some(after_brace) = rest.strip_prefix("^{") {
            let Some(close_idx) = after_brace.find('}') else {
                return Err(GirError::InvalidRevision(spec.to_string()));
            };
            let target_type = &after_brace[..close_idx];
            rest = &after_brace[close_idx + 1..];

            current_oid = peel_to_target_type(repo, &current_oid, target_type, spec)?;
        } else if let Some(after_caret) = rest.strip_prefix('^') {
            let (digits, remaining) = take_digits(after_caret);
            let parent_num: usize = if digits.is_empty() {
                1
            } else {
                digits
                    .parse()
                    .map_err(|_| GirError::InvalidRevision(spec.to_string()))?
            };
            rest = remaining;

            let (commit_oid, commit) = peel_to_commit(repo, &current_oid, spec)?;
            if parent_num == 0 {
                current_oid = commit_oid;
            } else {
                let Some(&parent_oid) = commit.parents.get(parent_num - 1) else {
                    return Err(GirError::InvalidRevision(spec.to_string()));
                };
                current_oid = parent_oid;
            }
        } else if let Some(path_str) = rest.strip_prefix(':') {
            rest = "";
            let tree_oid = peel_to_target_type(repo, &current_oid, "tree", spec)?;
            current_oid = lookup_path_in_tree(repo, &tree_oid, path_str, spec)?;
        } else {
            return Err(GirError::InvalidRevision(spec.to_string()));
        }
    }

    Ok(current_oid)
}

fn split_base_and_suffixes(spec: &str) -> (&str, &str) {
    if spec.starts_with("@{") {
        return ("", spec);
    }

    let bytes = spec.as_bytes();
    for i in 1..bytes.len() {
        match bytes[i] {
            b'~' | b'^' | b':' => return (&spec[..i], &spec[i..]),
            b'@' if spec[i..].starts_with("@{") => return (&spec[..i], &spec[i..]),
            _ => {}
        }
    }

    (spec, "")
}

fn take_digits(s: &str) -> (&str, &str) {
    let idx = s
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(s.len());
    (&s[..idx], &s[idx..])
}

fn resolve_base_identifier(repo: &Repository, base: &str) -> Result<ObjectId, GirError> {
    if let Ok((_, oid)) = repo.refs().resolve_dwim(base) {
        return Ok(oid);
    }
    repo.odb()
        .resolve_prefix(base)
        .map_err(|_| GirError::InvalidRevision(base.to_string()))
}

fn peel_to_commit(
    repo: &Repository,
    oid: &ObjectId,
    spec: &str,
) -> Result<(ObjectId, crate::object::commit::Commit), GirError> {
    let mut current = *oid;
    for _ in 0..10 {
        match repo.odb().read_parsed(&current)? {
            ParsedObject::Commit(c) => return Ok((current, c)),
            ParsedObject::Tag(t) => {
                current = t.object;
            }
            _ => return Err(GirError::InvalidRevision(spec.to_string())),
        }
    }
    Err(GirError::InvalidRevision(spec.to_string()))
}

fn peel_to_target_type(
    repo: &Repository,
    oid: &ObjectId,
    target_type: &str,
    spec: &str,
) -> Result<ObjectId, GirError> {
    let mut current = *oid;
    for _ in 0..10 {
        let parsed = repo.odb().read_parsed(&current)?;
        match (target_type, parsed) {
            ("", ParsedObject::Tag(t)) => {
                current = t.object;
            }
            ("", _) => return Ok(current),
            ("object", _) => return Ok(current),
            ("commit", ParsedObject::Commit(_)) => return Ok(current),
            ("commit", ParsedObject::Tag(t)) => {
                current = t.object;
            }
            ("tree", ParsedObject::Tree(_)) => return Ok(current),
            ("tree", ParsedObject::Commit(c)) => return Ok(c.tree),
            ("tree", ParsedObject::Tag(t)) => {
                current = t.object;
            }
            ("blob", ParsedObject::Blob(_)) => return Ok(current),
            ("blob", ParsedObject::Tag(t)) => {
                current = t.object;
            }
            ("tag", ParsedObject::Tag(_)) => return Ok(current),
            _ => return Err(GirError::InvalidRevision(spec.to_string())),
        }
    }
    Err(GirError::InvalidRevision(spec.to_string()))
}

fn lookup_path_in_tree(
    repo: &Repository,
    root_tree_oid: &ObjectId,
    path: &str,
    spec: &str,
) -> Result<ObjectId, GirError> {
    if path.is_empty() {
        return Ok(*root_tree_oid);
    }

    let mut current_oid = *root_tree_oid;
    let parts: Vec<&str> = path.split('/').filter(|p| !p.is_empty()).collect();
    for (idx, part) in parts.iter().enumerate() {
        let ParsedObject::Tree(tree) = repo.odb().read_parsed(&current_oid)? else {
            return Err(GirError::InvalidRevision(spec.to_string()));
        };
        let Some(entry) = tree.entries.iter().find(|e| e.name == *part) else {
            return Err(GirError::InvalidRevision(spec.to_string()));
        };
        current_oid = entry.oid;
        if idx + 1 < parts.len() && !entry.mode.is_dir() {
            return Err(GirError::InvalidRevision(spec.to_string()));
        }
    }

    Ok(current_oid)
}
