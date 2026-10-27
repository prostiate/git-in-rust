use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::io::Write;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::hooks::run_hook;
use crate::index::{flatten_tree, write_index_to_tree, GitIndex, IndexEntry};
use crate::object::commit::{Commit, Signature};
use crate::object::tree::FileMode;
use crate::object::{ObjectKind, ParsedObject};
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::storage::atomic::LockFile;
use crate::storage::ObjectDatabase;

/// Options for creating a commit via `create_commit`.
#[derive(Debug, Clone, Default)]
pub struct CommitOptions {
    pub message: String,
    pub all: bool,
    pub amend: bool,
    pub allow_empty: bool,
    pub no_verify: bool,
    pub author_override: Option<String>,
}

/// Summary of a newly created commit.
#[derive(Debug, Clone)]
pub struct CommitOutcome {
    pub commit_oid: ObjectId,
    pub branch_display: String,
    pub is_root: bool,
    pub subject: String,
    pub diff_summary: TreeDiffSummary,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileChangeKind {
    Added,
    Modified,
    Deleted,
}

#[derive(Debug, Clone)]
pub struct FileChangeStat {
    pub path: String,
    pub kind: FileChangeKind,
    pub old_mode: Option<FileMode>,
    pub new_mode: Option<FileMode>,
    pub old_oid: Option<ObjectId>,
    pub new_oid: Option<ObjectId>,
    pub insertions: usize,
    pub deletions: usize,
    pub is_binary: bool,
}

#[derive(Debug, Clone, Default)]
pub struct TreeDiffSummary {
    pub files: Vec<FileChangeStat>,
    pub total_insertions: usize,
    pub total_deletions: usize,
}

impl TreeDiffSummary {
    pub fn files_changed(&self) -> usize {
        self.files.len()
    }

    /// Formats the standard Git shortstat line:
    /// `" 2 files changed, 15 insertions(+), 3 deletions(-)"`
    pub fn format_shortstat(&self) -> String {
        let n = self.files.len();
        let file_word = if n == 1 { "file" } else { "files" };
        let mut parts = vec![format!(" {n} {file_word} changed")];
        if self.total_insertions > 0 {
            let ins_word = if self.total_insertions == 1 {
                "insertion(+)"
            } else {
                "insertions(+)"
            };
            parts.push(format!("{} {ins_word}", self.total_insertions));
        }
        if self.total_deletions > 0 {
            let del_word = if self.total_deletions == 1 {
                "deletion(-)"
            } else {
                "deletions(-)"
            };
            parts.push(format!("{} {del_word}", self.total_deletions));
        }
        parts.join(", ")
    }

    /// Formats the per-file `--stat` table plus the trailing shortstat line.
    pub fn format_stat_block(&self) -> String {
        if self.files.is_empty() {
            return String::new();
        }
        let max_path_len = self
            .files
            .iter()
            .map(|f| f.path.len())
            .max()
            .unwrap_or(1)
            .min(50);

        let mut out = String::new();
        for f in &self.files {
            if f.is_binary {
                out.push_str(&format!(
                    " {:<width$} | Bin\n",
                    f.path,
                    width = max_path_len
                ));
            } else {
                let total = f.insertions + f.deletions;
                let max_bars = 30usize;
                let (plus_bars, minus_bars) = if total <= max_bars {
                    (f.insertions, f.deletions)
                } else {
                    let p = (f.insertions * max_bars) / total;
                    let m = max_bars.saturating_sub(p);
                    (p, m)
                };
                let bars = format!("{}{}", "+".repeat(plus_bars), "-".repeat(minus_bars));
                out.push_str(&format!(
                    " {:<width$} | {:>3} {}\n",
                    f.path,
                    total,
                    bars,
                    width = max_path_len
                ));
            }
        }
        out.push_str(&self.format_shortstat());
        out.push('\n');
        out
    }
}

/// Executes the full porcelain `gir commit` transaction.
pub fn create_commit(repo: &Repository, opts: &CommitOptions) -> Result<CommitOutcome, GirError> {
    // 1. Run `pre-commit` hook unless `-n` / `--no-verify` is set
    if !opts.no_verify {
        run_hook(repo, "pre-commit", &[])?;
    }

    let mut index = GitIndex::read(repo.git_dir())?;

    // 2. If `-a` / `--all`, auto-stage modified and deleted tracked files
    if opts.all {
        let Some(worktree) = repo.worktree() else {
            return Err(GirError::InvalidArgument(
                "cannot use -a / --all in a bare repository".to_string(),
            ));
        };
        let tracked_paths: Vec<String> = index
            .entries()
            .filter(|e| e.stage() == 0)
            .map(|e| e.path.clone())
            .collect();

        for rel_path in tracked_paths {
            let abs = worktree.join(&rel_path);
            match fs::symlink_metadata(&abs) {
                Ok(meta) if !meta.file_type().is_dir() => {
                    let data = if meta.file_type().is_symlink() {
                        fs::read_link(&abs)?
                            .to_string_lossy()
                            .into_owned()
                            .into_bytes()
                    } else {
                        fs::read(&abs)?
                    };
                    let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &data)?;
                    let entry = IndexEntry::from_metadata(&rel_path, blob_oid, 0, &meta);
                    index.add_entry(entry);
                }
                _ => {
                    index.remove_path(&rel_path);
                }
            }
        }
    }

    // 3. Build root tree and update the index's `TREE` cache-tree extension
    let root_tree_oid = write_index_to_tree(&mut index, repo.odb())?;
    index.write(repo.git_dir())?;

    // 4. Resolve `HEAD` and `.git/MERGE_HEAD`
    let head_oid = repo.refs().resolve_to_oid("HEAD").ok();
    let head_commit = match head_oid {
        Some(oid) => match repo.odb().read_parsed(&oid)? {
            ParsedObject::Commit(c) => Some(c),
            _ => None,
        },
        None => None,
    };

    let merge_heads = read_merge_heads(repo)?;
    let is_merge = !merge_heads.is_empty() && !opts.amend;

    let parents: Vec<ObjectId> = if opts.amend {
        let Some(ref hc) = head_commit else {
            return Err(GirError::InvalidArgument(
                "You have nothing to amend.".to_string(),
            ));
        };
        hc.parents.clone()
    } else if let Some(h_oid) = head_oid {
        let mut p = vec![h_oid];
        for m_oid in &merge_heads {
            if !p.contains(m_oid) {
                p.push(*m_oid);
            }
        }
        p
    } else {
        Vec::new()
    };

    // 5. Empty commit guard (applies to both normal commits and `--amend` unless `--allow-empty` or merge)
    if !opts.allow_empty && !is_merge {
        if parents.is_empty() {
            if index.is_empty() {
                return Err(GirError::InvalidArgument(
                    "nothing to commit (create/copy files and use \"gir add\" to track)"
                        .to_string(),
                ));
            }
        } else if parents.len() == 1 {
            if let Ok(ParsedObject::Commit(parent_commit)) = repo.odb().read_parsed(&parents[0]) {
                if parent_commit.tree == root_tree_oid {
                    return Err(GirError::InvalidArgument(
                        "nothing to commit, working tree clean".to_string(),
                    ));
                }
            }
        }
    }

    // 6. Normalize commit message, write `.git/COMMIT_EDITMSG`, and run `commit-msg` hook
    let mut final_message = normalize_commit_message(&opts.message);
    let editmsg_path = repo.git_dir().join("COMMIT_EDITMSG");
    {
        let mut lock = LockFile::acquire(&editmsg_path)?;
        lock.write_all(final_message.as_bytes())?;
        lock.commit()?;
    }

    if !opts.no_verify {
        let arg_str = editmsg_path.to_string_lossy().into_owned();
        if run_hook(repo, "commit-msg", &[&arg_str])? {
            if let Ok(updated) = fs::read_to_string(&editmsg_path) {
                final_message = normalize_commit_message(&updated);
            }
        }
    }

    if final_message.trim().is_empty() {
        return Err(GirError::InvalidArgument(
            "Aborting commit due to empty commit message.".to_string(),
        ));
    }

    // 7. Resolve author & committer signatures
    let committer = repo.resolve_committer_signature()?;
    let author = if let Some(ref raw_author) = opts.author_override {
        let base_author = repo
            .resolve_author_signature()
            .unwrap_or_else(|_| committer.clone());
        let (name, email) = parse_author_name_email(raw_author)?;
        Signature::new(name, email, base_author.timestamp, base_author.tz_offset)
    } else if opts.amend
        && env::var_os("GIT_AUTHOR_NAME").is_none()
        && env::var_os("GIT_AUTHOR_EMAIL").is_none()
        && env::var_os("GIT_AUTHOR_DATE").is_none()
    {
        head_commit
            .as_ref()
            .map(|c| c.author.clone())
            .unwrap_or( repo.resolve_author_signature()?)
    } else {
        repo.resolve_author_signature()?
    };

    // 8. Write Commit object
    let commit = Commit {
        tree: root_tree_oid,
        parents: parents.clone(),
        author,
        committer: committer.clone(),
        message: final_message,
    };
    let commit_oid = repo.odb().write_object(&commit.to_raw())?;
    let subject = commit.subject().to_string();

    // 9. Update HEAD (and target branch) with reflog entry
    let is_root = parents.is_empty();
    let reflog_msg = if opts.amend {
        format!("commit (amend): {subject}")
    } else if is_root {
        format!("commit (initial): {subject}")
    } else if is_merge {
        format!("commit (merge): {subject}")
    } else {
        format!("commit: {subject}")
    };

    let expected_old = Some(head_oid.unwrap_or(ObjectId::ZERO));
    repo.refs()
        .update_ref("HEAD", commit_oid, expected_old, false, &committer, &reflog_msg)?;

    // Clean up merge state files if finishing a merge
    if is_merge {
        let _ = fs::remove_file(repo.git_dir().join("MERGE_HEAD"));
        let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    }

    // 10. Run `post-commit` hook (failures do not abort the completed commit)
    if !opts.no_verify {
        let _ = run_hook(repo, "post-commit", &[]);
    }

    // 11. Compute diff summary against first parent's tree (or empty tree if root commit)
    let parent_tree_oid = if let Some(first_parent_oid) = parents.first() {
        match repo.odb().read_parsed(first_parent_oid)? {
            ParsedObject::Commit(pc) => Some(pc.tree),
            _ => None,
        }
    } else {
        None
    };

    let diff_summary = diff_trees(repo.odb(), parent_tree_oid.as_ref(), &root_tree_oid)?;
    let branch_display = match repo.refs().read_raw("HEAD")? {
        Some(RefTarget::Symbolic(sym)) => sym
            .strip_prefix("refs/heads/")
            .unwrap_or(&sym)
            .to_string(),
        _ => "detached HEAD".to_string(),
    };

    Ok(CommitOutcome {
        commit_oid,
        branch_display,
        is_root,
        subject,
        diff_summary,
    })
}

fn read_merge_heads(repo: &Repository) -> Result<Vec<ObjectId>, GirError> {
    let path = repo.git_dir().join("MERGE_HEAD");
    if !path.is_file() {
        return Ok(Vec::new());
    }
    let content = fs::read_to_string(&path)?;
    let mut out = Vec::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if !trimmed.is_empty() {
            out.push(ObjectId::from_hex(trimmed)?);
        }
    }
    Ok(out)
}

pub fn normalize_commit_message(msg: &str) -> String {
    let mut cleaned = String::new();
    for line in msg.lines() {
        if line.starts_with('#') {
            continue;
        }
        cleaned.push_str(line.trim_end());
        cleaned.push('\n');
    }
    if cleaned.is_empty() && !msg.trim().is_empty() {
        cleaned.push_str(msg.trim());
        cleaned.push('\n');
    }
    cleaned
}

fn parse_author_name_email(raw: &str) -> Result<(String, String), GirError> {
    let trimmed = raw.trim();
    let Some(lt) = trimmed.find('<') else {
        return Err(GirError::InvalidArgument(format!(
            "invalid --author '{raw}': expected 'Name <email>'"
        )));
    };
    let Some(gt) = trimmed.rfind('>') else {
        return Err(GirError::InvalidArgument(format!(
            "invalid --author '{raw}': expected 'Name <email>'"
        )));
    };
    if gt <= lt {
        return Err(GirError::InvalidArgument(format!(
            "invalid --author '{raw}': expected 'Name <email>'"
        )));
    }
    let name = trimmed[..lt].trim().to_string();
    let email = trimmed[lt + 1..gt].trim().to_string();
    if name.is_empty() || email.is_empty() {
        return Err(GirError::InvalidArgument(format!(
            "invalid --author '{raw}': name and email must be non-empty"
        )));
    }
    Ok((name, email))
}

/// Computes file-level and line-level differences between `old_tree` (or empty tree) and `new_tree`.
pub fn diff_trees(
    odb: &ObjectDatabase,
    old_tree: Option<&ObjectId>,
    new_tree: &ObjectId,
) -> Result<TreeDiffSummary, GirError> {
    let old_map = match old_tree {
        Some(oid) => flatten_tree(odb, oid)?,
        None => BTreeMap::new(),
    };
    let new_map = flatten_tree(odb, new_tree)?;

    let mut all_paths: BTreeSet<&String> = BTreeSet::new();
    all_paths.extend(old_map.keys());
    all_paths.extend(new_map.keys());

    let mut files = Vec::new();
    let mut total_insertions = 0usize;
    let mut total_deletions = 0usize;

    for path in all_paths {
        let old_entry = old_map.get(path).copied();
        let new_entry = new_map.get(path).copied();

        match (old_entry, new_entry) {
            (None, Some((new_mode, new_oid))) => {
                let (ins, del, is_bin) = diff_blob_lines(odb, None, Some(&new_oid))?;
                total_insertions += ins;
                total_deletions += del;
                files.push(FileChangeStat {
                    path: path.clone(),
                    kind: FileChangeKind::Added,
                    old_mode: None,
                    new_mode: Some(new_mode),
                    old_oid: None,
                    new_oid: Some(new_oid),
                    insertions: ins,
                    deletions: del,
                    is_binary: is_bin,
                });
            }
            (Some((old_mode, old_oid)), None) => {
                let (ins, del, is_bin) = diff_blob_lines(odb, Some(&old_oid), None)?;
                total_insertions += ins;
                total_deletions += del;
                files.push(FileChangeStat {
                    path: path.clone(),
                    kind: FileChangeKind::Deleted,
                    old_mode: Some(old_mode),
                    new_mode: None,
                    old_oid: Some(old_oid),
                    new_oid: None,
                    insertions: ins,
                    deletions: del,
                    is_binary: is_bin,
                });
            }
            (Some((old_mode, old_oid)), Some((new_mode, new_oid))) => {
                if old_mode == new_mode && old_oid == new_oid {
                    continue;
                }
                let (ins, del, is_bin) = if old_oid == new_oid {
                    (0, 0, false)
                } else {
                    diff_blob_lines(odb, Some(&old_oid), Some(&new_oid))?
                };
                total_insertions += ins;
                total_deletions += del;
                files.push(FileChangeStat {
                    path: path.clone(),
                    kind: FileChangeKind::Modified,
                    old_mode: Some(old_mode),
                    new_mode: Some(new_mode),
                    old_oid: Some(old_oid),
                    new_oid: Some(new_oid),
                    insertions: ins,
                    deletions: del,
                    is_binary: is_bin,
                });
            }
            (None, None) => {}
        }
    }

    Ok(TreeDiffSummary {
        files,
        total_insertions,
        total_deletions,
    })
}

fn read_blob_bytes_opt(
    odb: &ObjectDatabase,
    oid: Option<&ObjectId>,
) -> Result<Option<Vec<u8>>, GirError> {
    let Some(oid) = oid else {
        return Ok(None);
    };
    let raw = odb.read_raw(oid)?;
    Ok(Some(raw.data))
}

fn is_binary_slice(bytes: &[u8]) -> bool {
    let check_len = bytes.len().min(8000);
    bytes[..check_len].contains(&0)
}

fn diff_blob_lines(
    odb: &ObjectDatabase,
    old_oid: Option<&ObjectId>,
    new_oid: Option<&ObjectId>,
) -> Result<(usize, usize, bool), GirError> {
    let old_bytes = read_blob_bytes_opt(odb, old_oid)?;
    let new_bytes = read_blob_bytes_opt(odb, new_oid)?;

    if old_bytes.as_deref().is_some_and(is_binary_slice)
        || new_bytes.as_deref().is_some_and(is_binary_slice)
    {
        return Ok((0, 0, true));
    }

    let old_text = old_bytes
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();
    let new_text = new_bytes
        .as_deref()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .unwrap_or_default();

    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    if old_lines.is_empty() {
        return Ok((new_lines.len(), 0, false));
    }
    if new_lines.is_empty() {
        return Ok((0, old_lines.len(), false));
    }

    let edits = compute_line_edits(&old_lines, &new_lines);
    let mut ins = 0usize;
    let mut del = 0usize;
    for edit in edits {
        match edit {
            LineEdit::Insert(_) => ins += 1,
            LineEdit::Delete(_) => del += 1,
            LineEdit::Equal(_) => {}
        }
    }
    Ok((ins, del, false))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LineEdit<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Computes a sequence of line edits (`Equal`, `Delete`, `Insert`) between `old_lines` and `new_lines`.
pub fn compute_line_edits<'a>(old_lines: &[&'a str], new_lines: &[&'a str]) -> Vec<LineEdit<'a>> {
    // Trim common prefix and suffix first for fast O(N) handling on typical edits
    let mut start = 0usize;
    while start < old_lines.len()
        && start < new_lines.len()
        && old_lines[start] == new_lines[start]
    {
        start += 1;
    }

    let mut old_end = old_lines.len();
    let mut new_end = new_lines.len();
    while old_end > start
        && new_end > start
        && old_lines[old_end - 1] == new_lines[new_end - 1]
    {
        old_end -= 1;
        new_end -= 1;
    }

    let mut out = Vec::with_capacity(old_lines.len() + new_lines.len());
    for &line in &old_lines[..start] {
        out.push(LineEdit::Equal(line));
    }

    let mid_old = &old_lines[start..old_end];
    let mid_new = &new_lines[start..new_end];

    let n = mid_old.len();
    let m = mid_new.len();

    if n == 0 {
        for &line in mid_new {
            out.push(LineEdit::Insert(line));
        }
    } else if m == 0 {
        for &line in mid_old {
            out.push(LineEdit::Delete(line));
        }
    } else if n.saturating_mul(m) <= 4_000_000 {
        // Exact DP LCS on the differing middle slice
        let width = m + 1;
        let mut dp = vec![0u32; (n + 1) * width];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i * width + j] = if mid_old[i] == mid_new[j] {
                    dp[(i + 1) * width + (j + 1)] + 1
                } else {
                    dp[(i + 1) * width + j].max(dp[i * width + (j + 1)])
                };
            }
        }
        let mut i = 0;
        let mut j = 0;
        while i < n && j < m {
            if mid_old[i] == mid_new[j] {
                out.push(LineEdit::Equal(mid_old[i]));
                i += 1;
                j += 1;
            } else if dp[(i + 1) * width + j] >= dp[i * width + (j + 1)] {
                out.push(LineEdit::Delete(mid_old[i]));
                i += 1;
            } else {
                out.push(LineEdit::Insert(mid_new[j]));
                j += 1;
            }
        }
        while i < n {
            out.push(LineEdit::Delete(mid_old[i]));
            i += 1;
        }
        while j < m {
            out.push(LineEdit::Insert(mid_new[j]));
            j += 1;
        }
    } else {
        for &line in mid_old {
            out.push(LineEdit::Delete(line));
        }
        for &line in mid_new {
            out.push(LineEdit::Insert(line));
        }
    }

    for &line in &old_lines[old_end..] {
        out.push(LineEdit::Equal(line));
    }

    out
}

/// Formats a unified diff patch (`diff --git a/<path> b/<path>`) for a `FileChangeStat`.
pub fn format_unified_file_patch(
    odb: &ObjectDatabase,
    change: &FileChangeStat,
) -> Result<String, GirError> {
    let mut out = String::new();
    out.push_str(&format!("diff --git a/{0} b/{0}\n", change.path));

    let short_old = change
        .old_oid
        .map(|id| id.short_hex())
        .unwrap_or_else(|| "0000000".to_string());
    let short_new = change
        .new_oid
        .map(|id| id.short_hex())
        .unwrap_or_else(|| "0000000".to_string());

    match change.kind {
        FileChangeKind::Added => {
            if let Some(mode) = change.new_mode {
                out.push_str(&format!("new file mode {mode}\n"));
            }
            out.push_str(&format!("index {short_old}..{short_new}\n"));
            out.push_str("--- /dev/null\n");
            out.push_str(&format!("+++ b/{}\n", change.path));
        }
        FileChangeKind::Deleted => {
            if let Some(mode) = change.old_mode {
                out.push_str(&format!("deleted file mode {mode}\n"));
            }
            out.push_str(&format!("index {short_old}..{short_new}\n"));
            out.push_str(&format!("--- a/{}\n", change.path));
            out.push_str("+++ /dev/null\n");
        }
        FileChangeKind::Modified => {
            let mode_str = change
                .new_mode
                .map(|m| format!(" {m}"))
                .unwrap_or_default();
            out.push_str(&format!("index {short_old}..{short_new}{mode_str}\n"));
            out.push_str(&format!("--- a/{}\n", change.path));
            out.push_str(&format!("+++ b/{}\n", change.path));
        }
    }

    if change.is_binary {
        out.push_str(&format!(
            "Binary files a/{0} and b/{0} differ\n",
            change.path
        ));
        return Ok(out);
    }

    let old_bytes = read_blob_bytes_opt(odb, change.old_oid.as_ref())?.unwrap_or_default();
    let new_bytes = read_blob_bytes_opt(odb, change.new_oid.as_ref())?.unwrap_or_default();
    let old_text = String::from_utf8_lossy(&old_bytes);
    let new_text = String::from_utf8_lossy(&new_bytes);
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    if old_lines.is_empty() && new_lines.is_empty() {
        return Ok(out);
    }

    let edits = compute_line_edits(&old_lines, &new_lines);
    out.push_str(&format_unified_hunks(&edits, 3));

    Ok(out)
}

/// Groups `LineEdit` items into unified diff hunks with `context_lines` (typically 3) of surrounding context.
pub fn format_unified_hunks(edits: &[LineEdit<'_>], context_lines: usize) -> String {
    let mut change_indices = Vec::new();
    for (idx, edit) in edits.iter().enumerate() {
        if !matches!(edit, LineEdit::Equal(_)) {
            change_indices.push(idx);
        }
    }
    if change_indices.is_empty() {
        return String::new();
    }

    // Build merged `[start_idx, end_idx)` ranges of `edits`
    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for idx in change_indices {
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines + 1).min(edits.len());
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    // Precompute prefix old/new line numbers before each edit index (0-based line counts)
    let mut old_before = vec![0usize; edits.len() + 1];
    let mut new_before = vec![0usize; edits.len() + 1];
    for (i, edit) in edits.iter().enumerate() {
        old_before[i + 1] = old_before[i]
            + usize::from(matches!(edit, LineEdit::Equal(_) | LineEdit::Delete(_)));
        new_before[i + 1] = new_before[i]
            + usize::from(matches!(edit, LineEdit::Equal(_) | LineEdit::Insert(_)));
    }

    let mut out = String::new();
    for (start, end) in ranges {
        let old_count = old_before[end] - old_before[start];
        let new_count = new_before[end] - new_before[start];
        let old_start = if old_count == 0 {
            old_before[start]
        } else {
            old_before[start] + 1
        };
        let new_start = if new_count == 0 {
            new_before[start]
        } else {
            new_before[start] + 1
        };

        out.push_str(&format!(
            "@@ -{},{} +{},{} @@\n",
            old_start, old_count, new_start, new_count
        ));
        for edit in &edits[start..end] {
            match edit {
                LineEdit::Equal(l) => {
                    out.push(' ');
                    out.push_str(l);
                    out.push('\n');
                }
                LineEdit::Delete(l) => {
                    out.push('-');
                    out.push_str(l);
                    out.push('\n');
                }
                LineEdit::Insert(l) => {
                    out.push('+');
                    out.push_str(l);
                    out.push('\n');
                }
            }
        }
    }

    out
}
