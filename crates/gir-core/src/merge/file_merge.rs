use crate::diff::hunk::is_binary_blob;
use crate::diff::myers::{myers_diff, EditOp};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ConflictStyle {
    #[default]
    Merge,
    Diff3,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeLabels<'a> {
    pub ours: &'a str,
    pub base: &'a str,
    pub theirs: &'a str,
}

impl<'a> Default for MergeLabels<'a> {
    fn default() -> Self {
        Self {
            ours: "HEAD",
            base: "base",
            theirs: "theirs",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergeFileResult {
    pub content: Vec<u8>,
    pub has_conflicts: bool,
    pub is_binary_conflict: bool,
}

#[derive(Debug, Clone)]
struct ChangeSpan<'a> {
    base_start: usize,
    base_end: usize,
    replacement: Vec<&'a str>,
}

/// Performs a 3-way (`diff3`) merge of `base`, `ours`, and `theirs` blob byte slices.
///
/// Non-overlapping line changes from `ours` and `theirs` are merged cleanly.
/// Overlapping changes with identical replacements also resolve cleanly; differing overlapping
/// changes emit standard `<<<<<<<` / `=======` / `>>>>>>>` conflict markers.
pub fn merge_blobs_3way(
    base: &[u8],
    ours: &[u8],
    theirs: &[u8],
    labels: MergeLabels<'_>,
    style: ConflictStyle,
) -> MergeFileResult {
    // Fast paths for byte-identical inputs
    if ours == theirs {
        return MergeFileResult {
            content: ours.to_vec(),
            has_conflicts: false,
            is_binary_conflict: false,
        };
    }
    if base == ours {
        return MergeFileResult {
            content: theirs.to_vec(),
            has_conflicts: false,
            is_binary_conflict: false,
        };
    }
    if base == theirs {
        return MergeFileResult {
            content: ours.to_vec(),
            has_conflicts: false,
            is_binary_conflict: false,
        };
    }

    // If any of the three blobs is binary and both `ours != base` and `theirs != base` with `ours != theirs`,
    // keep `ours` in working tree bytes and flag as a binary conflict.
    if is_binary_blob(base) || is_binary_blob(ours) || is_binary_blob(theirs) {
        return MergeFileResult {
            content: ours.to_vec(),
            has_conflicts: true,
            is_binary_conflict: true,
        };
    }

    let base_text = String::from_utf8_lossy(base);
    let ours_text = String::from_utf8_lossy(ours);
    let theirs_text = String::from_utf8_lossy(theirs);

    let base_lines: Vec<&str> = base_text.lines().collect();
    let ours_lines: Vec<&str> = ours_text.lines().collect();
    let theirs_lines: Vec<&str> = theirs_text.lines().collect();

    let spans_o = extract_change_spans(&base_lines, &ours_lines);
    let spans_t = extract_change_spans(&base_lines, &theirs_lines);

    let mut out_lines: Vec<String> = Vec::new();
    let mut has_conflicts = false;
    let mut b_pos = 0usize;
    let mut io = 0usize;
    let mut it = 0usize;

    while io < spans_o.len() || it < spans_t.len() {
        let next_o_start = spans_o.get(io).map(|s| s.base_start).unwrap_or(usize::MAX);
        let next_t_start = spans_t.get(it).map(|s| s.base_start).unwrap_or(usize::MAX);
        let next_start = next_o_start.min(next_t_start);

        if b_pos < next_start {
            for &line in &base_lines[b_pos..next_start] {
                out_lines.push(line.to_string());
            }
            b_pos = next_start;
        }

        // Check if only O is active before T starts
        if io < spans_o.len() && (it >= spans_t.len() || spans_o[io].base_end < spans_t[it].base_start) {
            for &line in &spans_o[io].replacement {
                out_lines.push(line.to_string());
            }
            b_pos = spans_o[io].base_end;
            io += 1;
            continue;
        }

        // Check if only T is active before O starts
        if it < spans_t.len() && (io >= spans_o.len() || spans_t[it].base_end < spans_o[io].base_start) {
            for &line in &spans_t[it].replacement {
                out_lines.push(line.to_string());
            }
            b_pos = spans_t[it].base_end;
            it += 1;
            continue;
        }

        // Otherwise `spans_o[io]` and `spans_t[it]` overlap (or touch at `[b_pos, ...)`).
        // Absorb all transitively overlapping spans from both sides into `[win_start, win_end)`.
        let win_start = b_pos;
        let mut win_end = spans_o[io].base_end.max(spans_t[it].base_end);
        let io_start = io;
        let it_start = it;
        io += 1;
        it += 1;

        loop {
            let mut grew = false;
            while io < spans_o.len() && spans_o[io].base_start <= win_end {
                win_end = win_end.max(spans_o[io].base_end);
                io += 1;
                grew = true;
            }
            while it < spans_t.len() && spans_t[it].base_start <= win_end {
                win_end = win_end.max(spans_t[it].base_end);
                it += 1;
                grew = true;
            }
            if !grew {
                break;
            }
        }

        let ours_chunk = render_window(&base_lines, win_start, win_end, &spans_o[io_start..io]);
        let theirs_chunk = render_window(&base_lines, win_start, win_end, &spans_t[it_start..it]);

        if ours_chunk == theirs_chunk {
            for line in ours_chunk {
                out_lines.push(line.to_string());
            }
        } else {
            has_conflicts = true;
            out_lines.push(format!("<<<<<<< {}", labels.ours));
            for line in ours_chunk {
                out_lines.push(line.to_string());
            }
            if style == ConflictStyle::Diff3 {
                out_lines.push(format!("||||||| {}", labels.base));
                for &line in &base_lines[win_start..win_end] {
                    out_lines.push(line.to_string());
                }
            }
            out_lines.push("=======".to_string());
            for line in theirs_chunk {
                out_lines.push(line.to_string());
            }
            out_lines.push(format!(">>>>>>> {}", labels.theirs));
        }

        b_pos = win_end;
    }

    if b_pos < base_lines.len() {
        for &line in &base_lines[b_pos..] {
            out_lines.push(line.to_string());
        }
    }

    let mut content = out_lines.join("\n").into_bytes();
    let had_trailing_newline = ours.ends_with(b"\n") || theirs.ends_with(b"\n") || base.ends_with(b"\n");
    if !content.is_empty() && had_trailing_newline {
        content.push(b'\n');
    }

    MergeFileResult {
        content,
        has_conflicts,
        is_binary_conflict: false,
    }
}

fn extract_change_spans<'a>(base_lines: &[&'a str], side_lines: &[&'a str]) -> Vec<ChangeSpan<'a>> {
    let edits = myers_diff(base_lines, side_lines);
    let mut spans = Vec::new();
    let mut b_idx = 0usize;
    let mut current: Option<ChangeSpan<'a>> = None;

    for op in edits {
        match op {
            EditOp::Equal(_) => {
                if let Some(span) = current.take() {
                    spans.push(span);
                }
                b_idx += 1;
            }
            EditOp::Delete(_) => {
                let span = current.get_or_insert_with(|| ChangeSpan {
                    base_start: b_idx,
                    base_end: b_idx,
                    replacement: Vec::new(),
                });
                span.base_end += 1;
                b_idx += 1;
            }
            EditOp::Insert(line) => {
                let span = current.get_or_insert_with(|| ChangeSpan {
                    base_start: b_idx,
                    base_end: b_idx,
                    replacement: Vec::new(),
                });
                span.replacement.push(line);
            }
        }
    }

    if let Some(span) = current.take() {
        spans.push(span);
    }

    spans
}

fn render_window<'a>(
    base_lines: &[&'a str],
    win_start: usize,
    win_end: usize,
    spans: &[ChangeSpan<'a>],
) -> Vec<&'a str> {
    let mut out = Vec::new();
    let mut pos = win_start;
    for span in spans {
        if pos < span.base_start {
            out.extend_from_slice(&base_lines[pos..span.base_start]);
        }
        out.extend_from_slice(&span.replacement);
        pos = span.base_end;
    }
    if pos < win_end {
        out.extend_from_slice(&base_lines[pos..win_end]);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_overlapping_edits_merge_cleanly() {
        let base = b"line 1\nline 2\nline 3\nline 4\nline 5\n";
        let ours = b"line 1\nline 2 OURS\nline 3\nline 4\nline 5\n";
        let theirs = b"line 1\nline 2\nline 3\nline 4\nline 5 THEIRS\n";

        let res = merge_blobs_3way(base, ours, theirs, MergeLabels::default(), ConflictStyle::Merge);
        assert!(!res.has_conflicts);
        assert_eq!(
            String::from_utf8(res.content).unwrap(),
            "line 1\nline 2 OURS\nline 3\nline 4\nline 5 THEIRS\n"
        );
    }

    #[test]
    fn identical_overlapping_edits_resolve_cleanly() {
        let base = b"a\nb\nc\n";
        let ours = b"a\nSAME_CHANGE\nc\n";
        let theirs = b"a\nSAME_CHANGE\nc\n";

        let res = merge_blobs_3way(base, ours, theirs, MergeLabels::default(), ConflictStyle::Merge);
        assert!(!res.has_conflicts);
        assert_eq!(String::from_utf8(res.content).unwrap(), "a\nSAME_CHANGE\nc\n");
    }

    #[test]
    fn conflicting_overlapping_edits_emit_markers() {
        let base = b"header\ntarget\nfooter\n";
        let ours = b"header\ntarget OURS\nfooter\n";
        let theirs = b"header\ntarget THEIRS\nfooter\n";

        let res = merge_blobs_3way(
            base,
            ours,
            theirs,
            MergeLabels {
                ours: "HEAD",
                base: "base",
                theirs: "feature",
            },
            ConflictStyle::Merge,
        );
        assert!(res.has_conflicts);
        assert_eq!(
            String::from_utf8(res.content).unwrap(),
            "header\n<<<<<<< HEAD\ntarget OURS\n=======\ntarget THEIRS\n>>>>>>> feature\nfooter\n"
        );
    }
}
