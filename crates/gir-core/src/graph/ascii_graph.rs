use crate::hash::ObjectId;

/// Stateful columnar ASCII DAG renderer for `gir log --graph`.
#[derive(Debug, Default, Clone)]
pub struct AsciiGraphRenderer {
    columns: Vec<ObjectId>,
}

impl AsciiGraphRenderer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Steps the graph state for commit `oid` with `parents` and returns:
    /// - `commit_prefix`: prefix for the commit header line (e.g. `"* "` or `"| * "`)
    /// - `continuation_prefix`: prefix for subsequent body lines of this commit (e.g. `"| "` or `"| | "`)
    /// - `connector_lines`: optional branch fork/merge connector lines (e.g. `["|\\"]` or `["|/"]`)
    pub fn step(
        &mut self,
        oid: ObjectId,
        parents: &[ObjectId],
    ) -> (String, String, Vec<String>) {
        let col_idx = if let Some(pos) = self.columns.iter().position(|&c| c == oid) {
            pos
        } else {
            self.columns.push(oid);
            self.columns.len() - 1
        };

        // Build the commit node line (`*` at `col_idx`, `|` at other columns)
        let mut commit_prefix = String::new();
        for i in 0..self.columns.len() {
            if i == col_idx {
                commit_prefix.push_str("* ");
            } else {
                commit_prefix.push_str("| ");
            }
        }

        // Remove duplicate lanes that converged onto `oid`
        let mut converged = false;
        let mut i = self.columns.len();
        while i > 0 {
            i -= 1;
            if i != col_idx && self.columns[i] == oid {
                self.columns.remove(i);
                converged = true;
            }
        }

        let new_col_idx = self
            .columns
            .iter()
            .position(|&c| c == oid)
            .unwrap_or(0);

        let mut connector_lines = Vec::new();

        if parents.is_empty() {
            self.columns.remove(new_col_idx);
        } else if parents.len() == 1 {
            let p0 = parents[0];
            if let Some(existing_idx) = self
                .columns
                .iter()
                .enumerate()
                .find_map(|(idx, &c)| (idx != new_col_idx && c == p0).then_some(idx))
            {
                self.columns.remove(new_col_idx);
                let merge_col = existing_idx.min(self.columns.len().saturating_sub(1));
                let mut line = String::new();
                for c in 0..self.columns.len() {
                    if c == merge_col {
                        line.push_str("|/");
                    } else {
                        line.push_str("| ");
                    }
                }
                if !line.is_empty() {
                    connector_lines.push(line);
                }
            } else {
                self.columns[new_col_idx] = p0;
                if converged && !self.columns.is_empty() {
                    let mut line = String::new();
                    for c in 0..self.columns.len() {
                        if c == new_col_idx {
                            line.push_str("|/");
                        } else {
                            line.push_str("| ");
                        }
                    }
                    connector_lines.push(line);
                }
            }
        } else {
            // Merge commit (`parents.len() >= 2`)
            self.columns[new_col_idx] = parents[0];
            let mut insert_at = new_col_idx + 1;
            let mut added_lane = false;
            for &extra_parent in &parents[1..] {
                if !self.columns.contains(&extra_parent) {
                    self.columns.insert(insert_at, extra_parent);
                    insert_at += 1;
                    added_lane = true;
                }
            }
            if added_lane {
                let mut line = String::new();
                for c in 0..self.columns.len() {
                    if c == new_col_idx {
                        line.push_str("|\\");
                    } else if c == new_col_idx + 1 {
                        line.push(' ');
                    } else {
                        line.push_str("| ");
                    }
                }
                connector_lines.push(line.trim_end().to_string());
            }
        }

        let mut continuation_prefix = String::new();
        for _ in 0..self.columns.len() {
            continuation_prefix.push_str("| ");
        }

        (commit_prefix, continuation_prefix, connector_lines)
    }
}
