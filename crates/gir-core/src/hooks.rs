use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::process::Command;

use crate::error::GirError;
use crate::repository::Repository;

/// Resolves the directory containing Git hooks for `repo`
/// (`core.hooksPath` in config if configured, otherwise `.git/hooks`).
pub fn resolve_hooks_dir(repo: &Repository) -> PathBuf {
    if let Ok(cfg) = repo.config() {
        if let Some(custom) = cfg.get("core.hookspath") {
            let path = PathBuf::from(custom.trim());
            if path.is_absolute() {
                return path;
            }
            let base = repo.worktree().unwrap_or_else(|| repo.git_dir());
            return base.join(path);
        }
    }
    repo.git_dir().join("hooks")
}

/// Executes a Git hook script (`pre-commit`, `commit-msg`, `post-commit`, etc.) if present and executable.
/// Returns `Ok(true)` if the hook existed and succeeded, `Ok(false)` if no executable hook existed,
/// or `Err` if the hook exited with a non-zero status code.
pub fn run_hook(repo: &Repository, hook_name: &str, args: &[&str]) -> Result<bool, GirError> {
    let hook_path = resolve_hooks_dir(repo).join(hook_name);
    let Ok(meta) = fs::metadata(&hook_path) else {
        return Ok(false);
    };
    if !meta.is_file() || (meta.permissions().mode() & 0o111) == 0 {
        return Ok(false);
    }

    let cwd = repo.worktree().unwrap_or_else(|| repo.git_dir());
    let status = Command::new(&hook_path)
        .args(args)
        .current_dir(cwd)
        .status()?;

    if !status.success() {
        return Err(GirError::InvalidArgument(format!(
            "hook '{hook_name}' exited with non-zero status ({})",
            status.code().unwrap_or(1)
        )));
    }

    Ok(true)
}
