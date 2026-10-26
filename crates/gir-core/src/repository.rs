use std::env;
use std::fs;
use std::io::Write;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::config::GitConfig;
use crate::error::GirError;
use crate::hash::ObjectId;
use crate::object::commit::Signature;
use crate::storage::atomic::LockFile;
use crate::storage::ObjectDatabase;

/// High-level handle to a Git repository on disk.
#[derive(Debug, Clone)]
pub struct Repository {
    /// Path to the working directory (None if bare repository).
    worktree: Option<PathBuf>,
    /// Path to the `.git` metadata directory.
    git_dir: PathBuf,
    /// Unified object database (`objects/`).
    odb: ObjectDatabase,
}

impl Repository {
    /// Initializes a new Git repository at `path` (`.git/` subdirectory if `!bare`, or directly in `path` if `bare`).
    pub fn init(path: impl AsRef<Path>, bare: bool) -> Result<Self, GirError> {
        let root = path.as_ref();
        fs::create_dir_all(root)?;
        let root = fs::canonicalize(root)?;

        let (worktree, git_dir) = if bare {
            (None, root.clone())
        } else {
            (Some(root.clone()), root.join(".git"))
        };

        // Create standard Git directory hierarchy
        fs::create_dir_all(git_dir.join("objects/info"))?;
        fs::create_dir_all(git_dir.join("objects/pack"))?;
        fs::create_dir_all(git_dir.join("refs/heads"))?;
        fs::create_dir_all(git_dir.join("refs/tags"))?;
        fs::create_dir_all(git_dir.join("logs/refs/heads"))?;
        fs::create_dir_all(git_dir.join("info"))?;
        fs::create_dir_all(git_dir.join("hooks"))?;

        // Determine initial default branch from global config (defaulting to "main")
        let global_cfg = GitConfig::load_cascaded(None).unwrap_or_default();
        let default_branch = global_cfg
            .get("init.defaultbranch")
            .unwrap_or("main")
            .trim();

        // Only write HEAD if it does not already exist (safe re-initialization)
        let head_path = git_dir.join("HEAD");
        if !head_path.exists() {
            let mut head_lock = LockFile::acquire(&head_path)?;
            writeln!(head_lock, "ref: refs/heads/{default_branch}")?;
            head_lock.commit()?;
        }

        // Write default .git/config if it does not already exist
        let config_path = git_dir.join("config");
        if !config_path.exists() {
            let mut cfg = GitConfig::new();
            cfg.set("core.repositoryformatversion", "0")?;
            cfg.set("core.filemode", "true")?;
            cfg.set("core.bare", if bare { "true" } else { "false" })?;
            if !bare {
                cfg.set("core.logallrefupdates", "true")?;
            }
            cfg.save_to(&config_path)?;
        }

        // Write default .git/description and .git/info/exclude if absent
        let desc_path = git_dir.join("description");
        if !desc_path.exists() {
            let mut lock = LockFile::acquire(&desc_path)?;
            writeln!(
                lock,
                "Unnamed repository; edit this file 'description' to name the repository."
            )?;
            lock.commit()?;
        }

        let exclude_path = git_dir.join("info/exclude");
        if !exclude_path.exists() {
            let mut lock = LockFile::acquire(&exclude_path)?;
            writeln!(lock, "# git ls-files --others --exclude-from=.git/info/exclude")?;
            lock.commit()?;
        }

        let odb = ObjectDatabase::new(git_dir.join("objects"));
        Ok(Self {
            worktree,
            git_dir,
            odb,
        })
    }

    /// Opens a repository given a known `git_dir` and optional `worktree`.
    pub fn open_from_git_dir(git_dir: PathBuf, worktree: Option<PathBuf>) -> Result<Self, GirError> {
        if !is_valid_git_dir(&git_dir) {
            return Err(GirError::NotARepository);
        }
        let odb = ObjectDatabase::new(git_dir.join("objects"));
        Ok(Self {
            worktree,
            git_dir,
            odb,
        })
    }

    /// Discovers a Git repository starting at `start_dir` and walking upward.
    /// Respects `GIT_DIR` and `GIT_CEILING_DIRECTORIES`.
    pub fn discover(start_dir: impl AsRef<Path>) -> Result<Self, GirError> {
        if let Ok(git_dir_env) = env::var("GIT_DIR") {
            let git_dir = PathBuf::from(git_dir_env);
            let worktree = git_dir.parent().map(Path::to_path_buf);
            return Self::open_from_git_dir(git_dir, worktree);
        }

        let mut current = fs::canonicalize(start_dir.as_ref()).map_err(|_| GirError::NotARepository)?;
        let start_dev = fs::metadata(&current).map(|m| m.dev()).ok();

        loop {
            let dot_git = current.join(".git");
            if dot_git.is_dir() && is_valid_git_dir(&dot_git) {
                return Self::open_from_git_dir(dot_git, Some(current));
            }

            // Also check if `current` itself is a bare repository
            if is_valid_git_dir(&current) {
                return Self::open_from_git_dir(current, None);
            }

            let Some(parent) = current.parent() else {
                return Err(GirError::NotARepository);
            };

            // Stop at filesystem device boundary unless GIT_DISCOVERY_ACROSS_FILESYSTEM is set
            if env::var_os("GIT_DISCOVERY_ACROSS_FILESYSTEM").is_none() {
                if let (Some(dev0), Ok(parent_meta)) = (start_dev, fs::metadata(parent)) {
                    if parent_meta.dev() != dev0 {
                        return Err(GirError::NotARepository);
                    }
                }
            }

            current = parent.to_path_buf();
        }
    }

    #[inline]
    pub fn git_dir(&self) -> &Path {
        &self.git_dir
    }

    #[inline]
    pub fn worktree(&self) -> Option<&Path> {
        self.worktree.as_deref()
    }

    #[inline]
    pub fn odb(&self) -> &ObjectDatabase {
        &self.odb
    }

    /// Loads the cascaded configuration (`~/.gitconfig` + `.git/config`) for this repository.
    pub fn config(&self) -> Result<GitConfig, GirError> {
        let local_path = self.git_dir.join("config");
        GitConfig::load_cascaded(Some(&local_path))
    }

    /// Loads only the repository-local `.git/config`.
    pub fn local_config(&self) -> Result<GitConfig, GirError> {
        GitConfig::load(self.git_dir.join("config"))
    }

    /// Resolves the author `Signature` from `GIT_AUTHOR_*` env vars or cascaded `GitConfig`.
    pub fn resolve_author_signature(&self) -> Result<Signature, GirError> {
        self.resolve_identity_with_env("GIT_AUTHOR_NAME", "GIT_AUTHOR_EMAIL", "GIT_AUTHOR_DATE")
    }

    /// Resolves the committer `Signature` from `GIT_COMMITTER_*` env vars or cascaded `GitConfig`.
    pub fn resolve_committer_signature(&self) -> Result<Signature, GirError> {
        self.resolve_identity_with_env(
            "GIT_COMMITTER_NAME",
            "GIT_COMMITTER_EMAIL",
            "GIT_COMMITTER_DATE",
        )
    }

    fn resolve_identity_with_env(
        &self,
        name_var: &str,
        email_var: &str,
        date_var: &str,
    ) -> Result<Signature, GirError> {
        let cfg = self.config()?;
        let name = env::var(name_var)
            .ok()
            .or_else(|| cfg.get("user.name").map(ToString::to_string))
            .ok_or(GirError::MissingIdentity)?;
        let email = env::var(email_var)
            .ok()
            .or_else(|| cfg.get("user.email").map(ToString::to_string))
            .ok_or(GirError::MissingIdentity)?;

        if let Ok(date_str) = env::var(date_var) {
            if let Some((ts, tz)) = Signature::parse_git_date(&date_str) {
                return Ok(Signature::new(name, email, ts, tz));
            }
        }

        Ok(Signature::now(name, email))
    }

    /// Resolves an object specification (`HEAD`, branch/tag ref, 40-char SHA, or short SHA prefix)
    /// to a concrete `ObjectId`.
    pub fn resolve_object(&self, spec: &str) -> Result<ObjectId, GirError> {
        let spec = spec.trim();

        // 1. Check `HEAD` or `@`
        if spec == "HEAD" || spec == "@" {
            return self.resolve_ref_to_oid("HEAD");
        }

        // 2. Check explicit/short ref names (`refs/...`, `heads/...`, `tags/...`)
        let candidate_refs = [
            spec.to_string(),
            format!("refs/{spec}"),
            format!("refs/tags/{spec}"),
            format!("refs/heads/{spec}"),
            format!("refs/remotes/{spec}"),
        ];
        for cand in &candidate_refs {
            if let Ok(oid) = self.resolve_ref_to_oid(cand) {
                return Ok(oid);
            }
        }

        // 3. Fall back to full or prefix hexadecimal ObjectId lookup
        self.odb.resolve_prefix(spec)
    }

    fn resolve_ref_to_oid(&self, ref_name: &str) -> Result<ObjectId, GirError> {
        let mut current_ref = ref_name.to_string();
        for _ in 0..10 {
            let ref_path = self.git_dir.join(&current_ref);
            if ref_path.is_file() {
                let content = fs::read_to_string(&ref_path)?;
                let trimmed = content.trim();
                if let Some(target) = trimmed.strip_prefix("ref:") {
                    current_ref = target.trim().to_string();
                    continue;
                }
                return ObjectId::from_hex(trimmed);
            }
            break;
        }
        Err(GirError::ObjectNotFound(ref_name.to_string()))
    }
}

fn is_valid_git_dir(dir: &Path) -> bool {
    dir.join("HEAD").is_file() && dir.join("objects").is_dir() && dir.join("refs").is_dir()
}
