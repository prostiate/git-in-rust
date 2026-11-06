use crate::diff::hunk::{DiffHunk, HunkLine};
use crate::error::GirError;

/// Applies a subset of selected `DiffHunk`s (in order of `old_start`) to `base_bytes`
/// and returns the resulting blob bytes.
///
/// Used by `gir add -p` (interactive hunk staging) and `gir tui` to stage individual hunks
/// into `.git/index` without modifying the working tree.
pub fn apply_hunks_to_blob(
    base_bytes: &[u8],
    selected_hunks: &[&DiffHunk],
) -> Result<Vec<u8>, GirError> {
    if selected_hunks.is_empty() {
        return Ok(base_bytes.to_vec());
    }

    let base_text = String::from_utf8_lossy(base_bytes);
    let base_lines: Vec<&str> = base_text.lines().collect();

    let mut out_lines: Vec<String> = Vec::new();
    let mut cursor = 0usize; // 0-based index in `base_lines`

    for hunk in selected_hunks {
        let hunk_old_idx = if hunk.old_start == 0 {
            0
        } else {
            hunk.old_start - 1
        };

        if hunk_old_idx < cursor || hunk_old_idx > base_lines.len() {
            return Err(GirError::InvalidArgument(format!(
                "patch hunk @@ -{},{} +{},{} @@ does not apply cleanly",
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count
            )));
        }

        // Copy unchanged lines prior to this hunk
        for &line in &base_lines[cursor..hunk_old_idx] {
            out_lines.push(line.to_string());
        }
        cursor = hunk_old_idx;

        // Apply hunk lines while verifying context and deleted lines match `base_lines`
        for hline in &hunk.lines {
            match hline {
                HunkLine::Context(expected) => {
                    let Some(&actual) = base_lines.get(cursor) else {
                        return Err(GirError::InvalidArgument(
                            "patch context extends past end of file".to_string(),
                        ));
                    };
                    if actual != expected {
                        return Err(GirError::InvalidArgument(format!(
                            "patch context mismatch at line {}: expected '{}', found '{}'",
                            cursor + 1,
                            expected,
                            actual
                        )));
                    }
                    out_lines.push(actual.to_string());
                    cursor += 1;
                }
                HunkLine::Delete(expected) => {
                    let Some(&actual) = base_lines.get(cursor) else {
                        return Err(GirError::InvalidArgument(
                            "patch deletion extends past end of file".to_string(),
                        ));
                    };
                    if actual != expected {
                        return Err(GirError::InvalidArgument(format!(
                            "patch deletion mismatch at line {}: expected '{}', found '{}'",
                            cursor + 1,
                            expected,
                            actual
                        )));
                    }
                    cursor += 1;
                }
                HunkLine::Insert(added) => {
                    out_lines.push(added.clone());
                }
            }
        }
    }

    // Copy remaining lines after the last applied hunk
    for &line in &base_lines[cursor..] {
        out_lines.push(line.to_string());
    }

    if out_lines.is_empty() {
        return Ok(Vec::new());
    }

    let mut result = out_lines.join("\n");
    result.push('\n');
    Ok(result.into_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::hunk::build_hunks_from_bytes;

    #[test]
    fn selective_hunk_application_for_add_patch() {
        let base: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let mut modified_lines: Vec<String> = (1..=20).map(|i| format!("line {i}")).collect();
        modified_lines[1] = "line 2 MODIFIED HUNK 1".to_string();
        modified_lines[18] = "line 19 MODIFIED HUNK 2".to_string();
        let modified = format!("{}\n", modified_lines.join("\n"));

        let (is_bin, hunks) = build_hunks_from_bytes(base.as_bytes(), modified.as_bytes(), 3);
        assert!(!is_bin);
        assert_eq!(hunks.len(), 2);

        // Apply ONLY hunk #2 (leaving hunk #1 unstaged!)
        let partial = apply_hunks_to_blob(base.as_bytes(), &[&hunks[1]]).unwrap();
        let partial_str = String::from_utf8(partial).unwrap();
        assert!(partial_str.contains("line 2\n"));
        assert!(!partial_str.contains("line 2 MODIFIED HUNK 1"));
        assert!(partial_str.contains("line 19 MODIFIED HUNK 2\n"));
    }
}
