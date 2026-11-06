use std::collections::HashMap;

/// A single line-level edit operation produced by the Myers $O(ND)$ Shortest Edit Script algorithm.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOp<'a> {
    Equal(&'a str),
    Delete(&'a str),
    Insert(&'a str),
}

/// Computes the Shortest Edit Script (SES) between `old_lines` and `new_lines`
/// using Eugene Myers' $O(ND)$ algorithm over interned `u32` line equivalence classes.
pub fn myers_diff<'a>(old_lines: &[&'a str], new_lines: &[&'a str]) -> Vec<EditOp<'a>> {
    // 1. Strip common prefix and suffix in O(N) time
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

    let mut result = Vec::with_capacity(old_lines.len() + new_lines.len());
    for &line in &old_lines[..start] {
        result.push(EditOp::Equal(line));
    }

    let mid_old = &old_lines[start..old_end];
    let mid_new = &new_lines[start..new_end];

    if mid_old.is_empty() {
        for &line in mid_new {
            result.push(EditOp::Insert(line));
        }
    } else if mid_new.is_empty() {
        for &line in mid_old {
            result.push(EditOp::Delete(line));
        }
    } else {
        // 2. Intern middle lines into u32 IDs for fast integer comparisons in the inner loop
        let mut interner: HashMap<&'a str, u32> =
            HashMap::with_capacity(mid_old.len() + mid_new.len());
        let mut next_id = 0u32;

        let a: Vec<u32> = mid_old
            .iter()
            .map(|&s| {
                *interner.entry(s).or_insert_with(|| {
                    let id = next_id;
                    next_id += 1;
                    id
                })
            })
            .collect();

        let b: Vec<u32> = mid_new
            .iter()
            .map(|&s| {
                *interner.entry(s).or_insert_with(|| {
                    let id = next_id;
                    next_id += 1;
                    id
                })
            })
            .collect();

        let mid_ops = myers_ses_tokens(&a, &b, mid_old, mid_new);
        result.extend(mid_ops);
    }

    for &line in &old_lines[old_end..] {
        result.push(EditOp::Equal(line));
    }

    normalize_change_runs(result)
}

fn myers_ses_tokens<'a>(
    a: &[u32],
    b: &[u32],
    old_lines: &[&'a str],
    new_lines: &[&'a str],
) -> Vec<EditOp<'a>> {
    let n = a.len() as isize;
    let m = b.len() as isize;
    let max_d = (n + m) as usize;

    // Guard against huge worst-case files where O((N+M)^2) trace memory would be excessive
    if max_d > 10_000 {
        let mut fallback = Vec::with_capacity(old_lines.len() + new_lines.len());
        for &l in old_lines {
            fallback.push(EditOp::Delete(l));
        }
        for &l in new_lines {
            fallback.push(EditOp::Insert(l));
        }
        return fallback;
    }

    let offset = max_d as isize;
    let mut v = vec![0isize; 2 * max_d + 1];
    let mut trace: Vec<Vec<isize>> = Vec::new();

    'outer: for d in 0..=(max_d as isize) {
        trace.push(v.clone());

        let mut k = -d;
        while k <= d {
            let k_idx = (k + offset) as usize;
            let mut x = if k == -d
                || (k != d && v[(k - 1 + offset) as usize] < v[(k + 1 + offset) as usize])
            {
                v[(k + 1 + offset) as usize]
            } else {
                v[(k - 1 + offset) as usize] + 1
            };
            let mut y = x - k;

            while x < n && y < m && a[x as usize] == b[y as usize] {
                x += 1;
                y += 1;
            }

            v[k_idx] = x;

            if x >= n && y >= m {
                break 'outer;
            }

            k += 2;
        }
    }

    // Backtrack from (n, m) to (0, 0)
    let mut rev_ops = Vec::with_capacity((n + m) as usize);
    let mut x = n;
    let mut y = m;

    for d in (1..trace.len()).rev() {
        let d_isize = d as isize;
        let v_prev = &trace[d];
        let k = x - y;

        let prev_k = if k == -d_isize
            || (k != d_isize
                && v_prev[(k - 1 + offset) as usize] < v_prev[(k + 1 + offset) as usize])
        {
            k + 1
        } else {
            k - 1
        };

        let prev_x = v_prev[(prev_k + offset) as usize];
        let prev_y = prev_x - prev_k;

        while x > prev_x && y > prev_y {
            rev_ops.push(EditOp::Equal(old_lines[(x - 1) as usize]));
            x -= 1;
            y -= 1;
        }

        if x == prev_x {
            rev_ops.push(EditOp::Insert(new_lines[(y - 1) as usize]));
            y -= 1;
        } else {
            rev_ops.push(EditOp::Delete(old_lines[(x - 1) as usize]));
            x -= 1;
        }
    }

    while x > 0 && y > 0 {
        rev_ops.push(EditOp::Equal(old_lines[(x - 1) as usize]));
        x -= 1;
        y -= 1;
    }

    rev_ops.reverse();
    rev_ops
}

/// Ensures that within every contiguous non-`Equal` change run, all `Delete` operations
/// appear before all `Insert` operations (canonical Git unified diff order).
fn normalize_change_runs<'a>(ops: Vec<EditOp<'a>>) -> Vec<EditOp<'a>> {
    let mut out = Vec::with_capacity(ops.len());
    let mut pending_deletes = Vec::new();
    let mut pending_inserts = Vec::new();

    for op in ops {
        match op {
            EditOp::Equal(line) => {
                out.append(&mut pending_deletes);
                out.append(&mut pending_inserts);
                out.push(EditOp::Equal(line));
            }
            EditOp::Delete(line) => pending_deletes.push(EditOp::Delete(line)),
            EditOp::Insert(line) => pending_inserts.push(EditOp::Insert(line)),
        }
    }
    out.append(&mut pending_deletes);
    out.append(&mut pending_inserts);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn myers_finds_shortest_edit_script() {
        let old = ["a", "b", "c", "a", "b", "b", "a"];
        let new = ["c", "b", "a", "b", "a", "c"];
        let edits = myers_diff(&old, &new);

        // Verify reconstructing `old` and `new` from `edits`
        let rec_old: Vec<&str> = edits
            .iter()
            .filter_map(|e| match e {
                EditOp::Equal(s) | EditOp::Delete(s) => Some(*s),
                EditOp::Insert(_) => None,
            })
            .collect();
        let rec_new: Vec<&str> = edits
            .iter()
            .filter_map(|e| match e {
                EditOp::Equal(s) | EditOp::Insert(s) => Some(*s),
                EditOp::Delete(_) => None,
            })
            .collect();

        assert_eq!(rec_old, old);
        assert_eq!(rec_new, new);

        // Minimum edit distance between ABCABBA (7) and CBABAC (6) is 5 (LCS length = 4: CBBA or CABA)
        let changes = edits
            .iter()
            .filter(|e| !matches!(e, EditOp::Equal(_)))
            .count();
        assert_eq!(changes, 5);
    }
}
