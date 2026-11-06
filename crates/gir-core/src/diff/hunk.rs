use std::fmt;

use crate::commit_ops::FileChangeKind;
use crate::diff::myers::{myers_diff, EditOp};
use crate::hash::ObjectId;
use crate::object::tree::FileMode;

/// A single line inside a unified `DiffHunk`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HunkLine {
    Context(String),
    Delete(String),
    Insert(String),
}

/// A 3-line-context unified diff hunk (`@@ -old_start,old_count +new_start,new_count @@`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffHunk {
    pub old_start: usize,
    pub old_count: usize,
    pub new_start: usize,
    pub new_count: usize,
    pub lines: Vec<HunkLine>,
}

impl fmt::Display for DiffHunk {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        writeln!(
            f,
            "@@ -{},{} +{},{} @@",
            self.old_start, self.old_count, self.new_start, self.new_count
        )?;
        for line in &self.lines {
            match line {
                HunkLine::Context(s) => writeln!(f, " {s}")?,
                HunkLine::Delete(s) => writeln!(f, "-{s}")?,
                HunkLine::Insert(s) => writeln!(f, "+{s}")?,
            }
        }
        Ok(())
    }
}

/// Complete diff representation for a single file (`diff --git a/<path> b/<path>`).
#[derive(Debug, Clone)]
pub struct FilePatch {
    pub path: String,
    pub kind: FileChangeKind,
    pub old_mode: Option<FileMode>,
    pub new_mode: Option<FileMode>,
    pub old_oid: Option<ObjectId>,
    pub new_oid: Option<ObjectId>,
    pub is_binary: bool,
    pub hunks: Vec<DiffHunk>,
}

impl FilePatch {
    pub fn insertions(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, HunkLine::Insert(_)))
            .count()
    }

    pub fn deletions(&self) -> usize {
        self.hunks
            .iter()
            .flat_map(|h| &h.lines)
            .filter(|l| matches!(l, HunkLine::Delete(_)))
            .count()
    }

    /// Formats the complete unified diff patch (`diff --git ...`) compatible with `git apply`.
    pub fn format_unified(&self) -> String {
        let mut out = String::new();
        out.push_str(&format!("diff --git a/{0} b/{0}\n", self.path));

        let short_old = self
            .old_oid
            .map(|id| id.short_hex())
            .unwrap_or_else(|| "0000000".to_string());
        let short_new = self
            .new_oid
            .map(|id| id.short_hex())
            .unwrap_or_else(|| "0000000".to_string());

        match self.kind {
            FileChangeKind::Added => {
                if let Some(mode) = self.new_mode {
                    out.push_str(&format!("new file mode {mode}\n"));
                }
                out.push_str(&format!("index {short_old}..{short_new}\n"));
                out.push_str("--- /dev/null\n");
                out.push_str(&format!("+++ b/{}\n", self.path));
            }
            FileChangeKind::Deleted => {
                if let Some(mode) = self.old_mode {
                    out.push_str(&format!("deleted file mode {mode}\n"));
                }
                out.push_str(&format!("index {short_old}..{short_new}\n"));
                out.push_str(&format!("--- a/{}\n", self.path));
                out.push_str("+++ /dev/null\n");
            }
            FileChangeKind::Modified => {
                if self.old_mode != self.new_mode {
                    if let Some(om) = self.old_mode {
                        out.push_str(&format!("old mode {om}\n"));
                    }
                    if let Some(nm) = self.new_mode {
                        out.push_str(&format!("new mode {nm}\n"));
                    }
                    if self.old_oid != self.new_oid {
                        out.push_str(&format!("index {short_old}..{short_new}\n"));
                    }
                } else {
                    let mode_str = self
                        .new_mode
                        .map(|m| format!(" {m}"))
                        .unwrap_or_default();
                    out.push_str(&format!("index {short_old}..{short_new}{mode_str}\n"));
                }
                if !self.hunks.is_empty() || self.is_binary {
                    out.push_str(&format!("--- a/{}\n", self.path));
                    out.push_str(&format!("+++ b/{}\n", self.path));
                }
            }
        }

        if self.is_binary {
            out.push_str(&format!(
                "Binary files a/{0} and b/{0} differ\n",
                self.path
            ));
            return out;
        }

        for hunk in &self.hunks {
            out.push_str(&hunk.to_string());
        }

        out
    }
}

/// Checks if the first 8,000 bytes of `data` contain a NUL (`0x00`) byte.
pub fn is_binary_blob(data: &[u8]) -> bool {
    let check_len = data.len().min(8000);
    data[..check_len].contains(&0)
}

/// Computes Myers diff between `old_bytes` and `new_bytes` and groups changes into `DiffHunk`s
/// with `context_lines` (default 3) of context.
pub fn build_hunks_from_bytes(
    old_bytes: &[u8],
    new_bytes: &[u8],
    context_lines: usize,
) -> (bool, Vec<DiffHunk>) {
    if is_binary_blob(old_bytes) || is_binary_blob(new_bytes) {
        return (true, Vec::new());
    }

    let old_text = String::from_utf8_lossy(old_bytes);
    let new_text = String::from_utf8_lossy(new_bytes);
    let old_lines: Vec<&str> = old_text.lines().collect();
    let new_lines: Vec<&str> = new_text.lines().collect();

    if old_lines == new_lines {
        return (false, Vec::new());
    }

    let ops = myers_diff(&old_lines, &new_lines);
    let hunks = group_ops_into_hunks(&ops, context_lines);
    (false, hunks)
}

pub fn group_ops_into_hunks(ops: &[EditOp<'_>], context_lines: usize) -> Vec<DiffHunk> {
    let mut change_indices = Vec::new();
    for (idx, op) in ops.iter().enumerate() {
        if !matches!(op, EditOp::Equal(_)) {
            change_indices.push(idx);
        }
    }
    if change_indices.is_empty() {
        return Vec::new();
    }

    let mut ranges: Vec<(usize, usize)> = Vec::new();
    for idx in change_indices {
        let start = idx.saturating_sub(context_lines);
        let end = (idx + context_lines + 1).min(ops.len());
        if let Some(last) = ranges.last_mut() {
            if start <= last.1 {
                last.1 = last.1.max(end);
                continue;
            }
        }
        ranges.push((start, end));
    }

    let mut old_before = vec![0usize; ops.len() + 1];
    let mut new_before = vec![0usize; ops.len() + 1];
    for (i, op) in ops.iter().enumerate() {
        old_before[i + 1] =
            old_before[i] + usize::from(matches!(op, EditOp::Equal(_) | EditOp::Delete(_)));
        new_before[i + 1] =
            new_before[i] + usize::from(matches!(op, EditOp::Equal(_) | EditOp::Insert(_)));
    }

    let mut hunks = Vec::with_capacity(ranges.len());
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

        let mut lines = Vec::with_capacity(end - start);
        for op in &ops[start..end] {
            match op {
                EditOp::Equal(s) => lines.push(HunkLine::Context((*s).to_string())),
                EditOp::Delete(s) => lines.push(HunkLine::Delete((*s).to_string())),
                EditOp::Insert(s) => lines.push(HunkLine::Insert((*s).to_string())),
            }
        }

        hunks.push(DiffHunk {
            old_start,
            old_count,
            new_start,
            new_count,
            lines,
        });
    }

    hunks
}
