pub mod snapshot;

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::Path;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::Signature;

/// A single entry in `.git/logs/<refname>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReflogEntry {
    pub old_oid: ObjectId,
    pub new_oid: ObjectId,
    pub committer: Signature,
    pub message: String,
}

impl ReflogEntry {
    /// Parses a single line from a Git reflog file:
    /// `<40-hex-old> <40-hex-new> <Name> <<Email>> <timestamp> <tz>\t<message>`
    pub fn parse(line: &str) -> Result<Self, GirError> {
        let line = line.trim_end_matches(&['\r', '\n'][..]);
        let (left, message) = match line.split_once('\t') {
            Some((l, m)) => (l, m.to_string()),
            None => (line, String::new()),
        };

        if left.len() < 83 {
            return Err(GirError::InvalidArgument(format!(
                "malformed reflog line: '{line}'"
            )));
        }

        let old_oid = ObjectId::from_hex(&left[0..40])?;
        let new_oid = ObjectId::from_hex(&left[41..81])?;
        let sig_str = &left[82..];
        let committer = Signature::parse("<reflog>", sig_str)?;

        Ok(Self {
            old_oid,
            new_oid,
            committer,
            message,
        })
    }

    /// Formats this reflog entry as a single newline-terminated line.
    pub fn serialize(&self) -> String {
        if self.message.is_empty() {
            format!("{} {} {}\n", self.old_oid, self.new_oid, self.committer)
        } else {
            format!(
                "{} {} {}\t{}\n",
                self.old_oid, self.new_oid, self.committer, self.message
            )
        }
    }
}

/// Reads all `ReflogEntry` records from `.git/logs/<ref_name>` in chronological order (oldest first).
pub fn read_reflog(git_dir: &Path, ref_name: &str) -> Result<Vec<ReflogEntry>, GirError> {
    let path = git_dir.join("logs").join(ref_name);
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    let mut entries = Vec::new();
    for line in content.lines() {
        if line.trim().is_empty() {
            continue;
        }
        entries.push(ReflogEntry::parse(line)?);
    }
    Ok(entries)
}

/// Resolves `<ref>@{index}` (where `0` is the newest entry) using Git's `read_ref_at` semantics.
pub fn resolve_reflog_at(
    git_dir: &Path,
    ref_name: &str,
    index: usize,
) -> Result<ObjectId, GirError> {
    let entries = read_reflog(git_dir, ref_name)?;
    if entries.is_empty() {
        return Err(GirError::InvalidRevision(format!("{ref_name}@{{{index}}}")));
    }

    let len = entries.len();
    if index < len {
        return Ok(entries[len - 1 - index].new_oid);
    }
    if index == len && !entries[0].old_oid.is_zero() {
        return Ok(entries[0].old_oid);
    }

    Err(GirError::InvalidRevision(format!(
        "log for '{ref_name}' only has {len} entries"
    )))
}

/// Resolves `@{-N}` (the N-th prior branch/commit checked out according to `.git/logs/HEAD`).
pub fn resolve_previous_checkout_branch(
    git_dir: &Path,
    nth: usize,
) -> Result<String, GirError> {
    if nth == 0 {
        return Err(GirError::InvalidRevision("@{-0}".to_string()));
    }
    let entries = read_reflog(git_dir, "HEAD")?;
    let mut count = 0usize;
    for entry in entries.iter().rev() {
        if let Some(rest) = entry.message.strip_prefix("checkout: moving from ") {
            if let Some((from_branch, _)) = rest.split_once(" to ") {
                count += 1;
                if count == nth {
                    return Ok(from_branch.trim().to_string());
                }
            }
        }
    }
    Err(GirError::InvalidRevision(format!("@{{-{nth}}}")))
}

/// Appends a single transition line to `.git/logs/<ref_name>`.
pub fn append_reflog(
    git_dir: &Path,
    ref_name: &str,
    old_oid: ObjectId,
    new_oid: ObjectId,
    committer: &Signature,
    message: &str,
) -> Result<(), GirError> {
    let path = git_dir.join("logs").join(ref_name);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = ReflogEntry {
        old_oid,
        new_oid,
        committer: committer.clone(),
        message: message.to_string(),
    };
    let mut file = OpenOptions::new().create(true).append(true).open(&path)?;
    file.write_all(entry.serialize().as_bytes())?;
    file.sync_all()?;
    Ok(())
}
