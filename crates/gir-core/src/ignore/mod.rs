pub mod pattern;

use std::env;
use std::fs;
use std::path::Path;

use crate::repository::Repository;
pub use pattern::{glob_match, IgnoreRule};

/// Hierarchical `.gitignore` rule stack evaluating `.git/info/exclude`, global ignore,
/// and per-directory `.gitignore` files from repository root downward.
#[derive(Debug, Clone, Default)]
pub struct IgnoreStack {
    rules: Vec<IgnoreRule>,
}

impl IgnoreStack {
    pub fn new() -> Self {
        Self { rules: Vec::new() }
    }

    /// Loads repository-wide ignore rules (`~/.config/git/ignore`, `.git/info/exclude`, and root `.gitignore`).
    pub fn for_repository(repo: &Repository) -> Self {
        let mut stack = Self::new();

        // 1. Global ignore file (`~/.config/git/ignore`)
        if let Some(home) = env::var_os("HOME") {
            let global_ignore = Path::new(&home).join(".config/git/ignore");
            if global_ignore.is_file() {
                stack.add_file("", &global_ignore);
            }
        }

        // 2. Repository `.git/info/exclude`
        let info_exclude = repo.git_dir().join("info/exclude");
        if info_exclude.is_file() {
            stack.add_file("", &info_exclude);
        }

        // 3. Root `.gitignore` in working tree
        if let Some(wt) = repo.worktree() {
            let root_gitignore = wt.join(".gitignore");
            if root_gitignore.is_file() {
                stack.add_file("", &root_gitignore);
            }
        }

        stack
    }

    /// Parses and appends rules from `file_path` scoped to `base_dir` (relative to worktree root).
    pub fn add_file(&mut self, base_dir: &str, file_path: &Path) {
        if let Ok(content) = fs::read_to_string(file_path) {
            self.add_str(base_dir, &content);
        }
    }

    /// Parses and appends rules from `content` scoped to `base_dir`.
    pub fn add_str(&mut self, base_dir: &str, content: &str) {
        for line in content.lines() {
            if let Some(rule) = IgnoreRule::parse(base_dir, line) {
                self.rules.push(rule);
            }
        }
    }

    /// Returns a new `IgnoreStack` with rules from `<dir>/.gitignore` appended (if present).
    pub fn enter_dir(&self, rel_dir: &str, abs_dir: &Path) -> Self {
        let mut child = self.clone();
        let gitignore = abs_dir.join(".gitignore");
        if gitignore.is_file() {
            child.add_file(rel_dir, &gitignore);
        }
        child
    }

    /// Evaluates whether `rel_path` (relative to worktree root, `/` separator) is ignored.
    /// Last matching rule in the stack wins (`!pattern` un-ignores a previously ignored path).
    pub fn is_ignored(&self, rel_path: &str, is_dir: bool) -> bool {
        let mut ignored = false;
        for rule in &self.rules {
            if rule.matches(rel_path, is_dir) {
                ignored = !rule.negated;
            }
        }
        ignored
    }
}
