/// A single parsed `.gitignore` rule scoped to a base directory relative to the repository worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IgnoreRule {
    /// Relative directory path (e.g. `""` for root `.gitignore`, `"src/sub"` for `src/sub/.gitignore`).
    pub base_dir: String,
    /// Raw pattern after stripping `!`, leading `/`, and trailing `/`.
    pub pattern: String,
    /// Whether this rule starts with `!` (un-ignores a previously ignored file).
    pub negated: bool,
    /// Whether this rule ends with `/` (matches only directories).
    pub dir_only: bool,
    /// Whether this rule is anchored to `base_dir` (had a leading `/` or internal `/` without leading `**/`).
    pub anchored: bool,
}

impl IgnoreRule {
    /// Parses a single line from a `.gitignore` file anchored at `base_dir`.
    /// Returns `None` for blank lines or comment lines (`#`).
    pub fn parse(base_dir: &str, raw_line: &str) -> Option<Self> {
        let line = strip_trailing_unescaped_spaces(raw_line.trim_end_matches(&['\r', '\n'][..]));
        if line.is_empty() {
            return None;
        }

        // Unescaped leading `#` is a comment
        if line.starts_with('#') {
            return None;
        }

        let mut s = line.as_str();
        let mut negated = false;
        if let Some(rest) = s.strip_prefix('!') {
            negated = true;
            s = rest;
        } else if let Some(rest) = s.strip_prefix(r"\!") {
            s = &line[1..];
            let _ = rest;
        } else if s.starts_with(r"\#") {
            s = &s[1..];
        }

        let mut dir_only = false;
        if s.ends_with('/') && !s.ends_with(r"\/") {
            dir_only = true;
            s = s.trim_end_matches('/');
        }

        if s.is_empty() {
            return None;
        }

        // Determine anchoring:
        // A leading `/` or an internal `/` (unless starting with `**/`) anchors the rule to `base_dir`.
        let anchored;
        if let Some(stripped) = s.strip_prefix('/') {
            anchored = true;
            s = stripped;
        } else if s.starts_with("**/") {
            anchored = false;
        } else {
            anchored = s.contains('/');
        }

        let clean_base = base_dir.trim_matches('/').to_string();

        Some(Self {
            base_dir: clean_base,
            pattern: unescape_gitignore(s),
            negated,
            dir_only,
            anchored,
        })
    }

    /// Checks whether this rule matches `rel_path` (relative to repository root, `/` separators)
    /// given `is_dir`.
    pub fn matches(&self, rel_path: &str, is_dir: bool) -> bool {
        if self.dir_only && !is_dir {
            return false;
        }

        let rel_path = rel_path.trim_matches('/');
        let subpath = if self.base_dir.is_empty() {
            rel_path
        } else if let Some(rest) = rel_path.strip_prefix(&self.base_dir) {
            if let Some(after_slash) = rest.strip_prefix('/') {
                after_slash
            } else {
                return false;
            }
        } else {
            return false;
        };

        if subpath.is_empty() {
            return false;
        }

        if self.anchored {
            glob_match(&self.pattern, subpath)
        } else {
            // Unanchored pattern: if pattern has no `/`, match against basename OR full subpath;
            // if pattern has `/` (e.g. `**/foo/bar`), match against full subpath or any suffix after `/`.
            if !self.pattern.contains('/') {
                let basename = subpath.rsplit('/').next().unwrap_or(subpath);
                if glob_match(&self.pattern, basename) {
                    return true;
                }
            }
            if glob_match(&self.pattern, subpath) {
                return true;
            }
            for (idx, ch) in subpath.char_indices() {
                if ch == '/' && glob_match(&self.pattern, &subpath[idx + 1..]) {
                    return true;
                }
            }
            false
        }
    }
}

fn strip_trailing_unescaped_spaces(s: &str) -> String {
    let mut bytes = s.as_bytes().to_vec();
    while bytes.last() == Some(&b' ') {
        if bytes.len() >= 2 && bytes[bytes.len() - 2] == b'\\' {
            // Escaped trailing space `\ `: remove the backslash and keep the space
            bytes.remove(bytes.len() - 2);
            break;
        }
        bytes.pop();
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn unescape_gitignore(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(&next) = chars.peek() {
                out.push(next);
                chars.next();
                continue;
            }
        }
        out.push(c);
    }
    out
}

/// Matches a `.gitignore` glob pattern (supporting `*`, `?`, `[...]`, `**/`, `/**`, `/**/`) against `text`.
pub fn glob_match(pattern: &str, text: &str) -> bool {
    glob_match_bytes(pattern.as_bytes(), text.as_bytes())
}

fn glob_match_bytes(pat: &[u8], txt: &[u8]) -> bool {
    if pat.is_empty() {
        return txt.is_empty();
    }

    // Handle `/**` at the end of pattern: matches one or more path characters inside directory
    if pat == b"**" {
        return true;
    }
    if let Some(after_doublestar_slash) = pat.strip_prefix(b"**/") {
        // `**/foo` matches `foo` at root or after any `/`
        if glob_match_bytes(after_doublestar_slash, txt) {
            return true;
        }
        for i in 0..txt.len() {
            if txt[i] == b'/' && glob_match_bytes(after_doublestar_slash, &txt[i + 1..]) {
                return true;
            }
        }
        return false;
    }

    match pat[0] {
        b'*' => {
            if pat.starts_with(b"**") {
                // Multi-segment `**`
                let rest = &pat[2..];
                for i in 0..=txt.len() {
                    if glob_match_bytes(rest, &txt[i..]) {
                        return true;
                    }
                }
                false
            } else {
                // Single-segment `*` (cannot match `/`)
                let rest = &pat[1..];
                if glob_match_bytes(rest, txt) {
                    return true;
                }
                for i in 0..txt.len() {
                    if txt[i] == b'/' {
                        break;
                    }
                    if glob_match_bytes(rest, &txt[i + 1..]) {
                        return true;
                    }
                }
                false
            }
        }
        b'?' => {
            if txt.is_empty() || txt[0] == b'/' {
                false
            } else {
                glob_match_bytes(&pat[1..], &txt[1..])
            }
        }
        b'[' => {
            if txt.is_empty() || txt[0] == b'/' {
                return false;
            }
            if let Some((matched, next_pat)) = match_char_class(&pat[1..], txt[0]) {
                matched && glob_match_bytes(next_pat, &txt[1..])
            } else {
                pat[0] == txt[0] && glob_match_bytes(&pat[1..], &txt[1..])
            }
        }
        c => {
            if txt.is_empty() || txt[0] != c {
                false
            } else {
                glob_match_bytes(&pat[1..], &txt[1..])
            }
        }
    }
}

fn match_char_class(pat: &[u8], ch: u8) -> Option<(bool, &[u8])> {
    let close_pos = pat.iter().position(|&b| b == b']')?;
    let class = &pat[..close_pos];
    let next_pat = &pat[close_pos + 1..];

    let (negated, items) = if let Some(rest) = class.strip_prefix(b"!") {
        (true, rest)
    } else if let Some(rest) = class.strip_prefix(b"^") {
        (true, rest)
    } else {
        (false, class)
    };

    let mut found = false;
    let mut i = 0;
    while i < items.len() {
        if i + 2 < items.len() && items[i + 1] == b'-' {
            if ch >= items[i] && ch <= items[i + 2] {
                found = true;
            }
            i += 3;
        } else {
            if items[i] == ch {
                found = true;
            }
            i += 1;
        }
    }

    Some((if negated { !found } else { found }, next_pat))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gitignore_pattern_matching_rules() {
        let log_rule = IgnoreRule::parse("", "*.log").unwrap();
        assert!(log_rule.matches("debug.log", false));
        assert!(log_rule.matches("nested/dir/debug.log", false));
        assert!(!log_rule.matches("debug.txt", false));

        let neg_rule = IgnoreRule::parse("", "!important.log").unwrap();
        assert!(neg_rule.negated);
        assert!(neg_rule.matches("important.log", false));
        assert!(neg_rule.matches("nested/important.log", false));

        let dir_rule = IgnoreRule::parse("", "build/").unwrap();
        assert!(dir_rule.dir_only);
        assert!(dir_rule.matches("build", true));
        assert!(dir_rule.matches("sub/build", true));
        assert!(!dir_rule.matches("build", false));

        let anchored_rule = IgnoreRule::parse("", "/root.txt").unwrap();
        assert!(anchored_rule.anchored);
        assert!(anchored_rule.matches("root.txt", false));
        assert!(!anchored_rule.matches("sub/root.txt", false));

        let doublestar_rule = IgnoreRule::parse("", "**/temp/**").unwrap();
        assert!(doublestar_rule.matches("temp/a.txt", false));
        assert!(doublestar_rule.matches("src/temp/deep/b.txt", false));
    }
}
