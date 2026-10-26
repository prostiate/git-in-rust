use std::collections::{BTreeMap, HashSet, VecDeque};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

use rayon::prelude::*;

use crate::error::GirError;
use crate::hash::ObjectId;
use crate::ignore::IgnoreStack;
use crate::index::{flatten_tree, GitIndex, IndexEntry};
use crate::object::tree::FileMode;
use crate::object::{ObjectKind, ParsedObject, RawObject};
use crate::repository::Repository;

/// Kind of modification to a single path between two trees/states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeltaStatus {
    Added,
    Modified,
    Deleted,
    Conflicted,
}

impl DeltaStatus {
    pub const fn as_char(self) -> char {
        match self {
            Self::Added => 'A',
            Self::Modified => 'M',
            Self::Deleted => 'D',
            Self::Conflicted => 'U',
        }
    }
}

/// Combined index (`X`) and worktree (`Y`) status for a single path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileStatusEntry {
    pub path: String,
    pub index_status: Option<DeltaStatus>,
    pub worktree_status: Option<DeltaStatus>,
}

impl FileStatusEntry {
    /// Formats a porcelain v1 2-character status code (`XY <path>`).
    pub fn porcelain_line(&self) -> String {
        let x = self.index_status.map(DeltaStatus::as_char).unwrap_or(' ');
        let y = self.worktree_status.map(DeltaStatus::as_char).unwrap_or(' ');
        format!("{x}{y} {}", self.path)
    }
}

/// Upstream tracking summary (`origin/main`, ahead/behind counts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpstreamStatus {
    pub upstream_name: String,
    pub ahead: usize,
    pub behind: usize,
}

/// Options controlling `compute_status`.
#[derive(Debug, Clone, Copy, Default)]
pub struct StatusOptions {
    /// Expand untracked directories into individual file paths (`-uall`).
    pub untracked_all: bool,
    /// Include ignored files (`--ignored`).
    pub include_ignored: bool,
}

/// Full status report for a repository.
#[derive(Debug, Clone)]
pub struct StatusReport {
    /// Current branch short name (e.g. `"main"`), or `None` if detached `HEAD`.
    pub branch: Option<String>,
    /// Current `HEAD` commit OID (`None` if unborn branch).
    pub head_oid: Option<ObjectId>,
    /// Upstream tracking status if configured.
    pub upstream: Option<UpstreamStatus>,
    /// Tracked entries with staged and/or unstaged changes, sorted by path.
    pub entries: Vec<FileStatusEntry>,
    /// Untracked files (or collapsed untracked directories `dir/`), sorted by path.
    pub untracked: Vec<String>,
    /// Ignored files/directories (populated when `include_ignored` is `true`).
    pub ignored: Vec<String>,
}

impl StatusReport {
    /// Returns `true` if there are no staged, unstaged, or untracked changes.
    pub fn is_clean(&self) -> bool {
        self.entries.is_empty() && self.untracked.is_empty()
    }

    /// Formats `git status --porcelain=v1` (or `-s`) output.
    pub fn format_porcelain(&self, show_branch: bool) -> String {
        let mut out = String::new();
        if show_branch {
            match (&self.branch, &self.upstream) {
                (Some(b), Some(up)) => {
                    if up.ahead == 0 && up.behind == 0 {
                        out.push_str(&format!("## {b}...{}\n", up.upstream_name));
                    } else if up.ahead > 0 && up.behind > 0 {
                        out.push_str(&format!(
                            "## {b}...{} [ahead {}, behind {}]\n",
                            up.upstream_name, up.ahead, up.behind
                        ));
                    } else if up.ahead > 0 {
                        out.push_str(&format!(
                            "## {b}...{} [ahead {}]\n",
                            up.upstream_name, up.ahead
                        ));
                    } else {
                        out.push_str(&format!(
                            "## {b}...{} [behind {}]\n",
                            up.upstream_name, up.behind
                        ));
                    }
                }
                (Some(b), None) => {
                    out.push_str(&format!("## {b}\n"));
                }
                (None, _) => {
                    out.push_str("## HEAD (no branch)\n");
                }
            }
        }

        for entry in &self.entries {
            out.push_str(&entry.porcelain_line());
            out.push('\n');
        }
        for u in &self.untracked {
            out.push_str(&format!("?? {u}\n"));
        }
        for ig in &self.ignored {
            out.push_str(&format!("!! {ig}\n"));
        }
        out
    }

    /// Formats standard human-readable `git status` output.
    pub fn format_long(&self) -> String {
        let mut out = String::new();
        match &self.branch {
            Some(b) => out.push_str(&format!("On branch {b}\n")),
            None => {
                if let Some(oid) = self.head_oid {
                    out.push_str(&format!("HEAD detached at {}\n", &oid.to_hex()[..7]));
                } else {
                    out.push_str("Not currently on any branch.\n");
                }
            }
        }

        if let Some(ref up) = self.upstream {
            if up.ahead == 0 && up.behind == 0 {
                out.push_str(&format!(
                    "Your branch is up to date with '{}'.\n\n",
                    up.upstream_name
                ));
            } else if up.ahead > 0 && up.behind == 0 {
                out.push_str(&format!(
                    "Your branch is ahead of '{}' by {} commit(s).\n\n",
                    up.upstream_name, up.ahead
                ));
            } else if up.behind > 0 && up.ahead == 0 {
                out.push_str(&format!(
                    "Your branch is behind '{}' by {} commit(s).\n\n",
                    up.upstream_name, up.behind
                ));
            } else {
                out.push_str(&format!(
                    "Your branch and '{}' have diverged ({} ahead, {} behind).\n\n",
                    up.upstream_name, up.ahead, up.behind
                ));
            }
        } else if self.head_oid.is_none() {
            out.push_str("\nNo commits yet\n\n");
        }

        let staged: Vec<&FileStatusEntry> = self
            .entries
            .iter()
            .filter(|e| e.index_status.is_some() && e.index_status != Some(DeltaStatus::Conflicted))
            .collect();
        let conflicted: Vec<&FileStatusEntry> = self
            .entries
            .iter()
            .filter(|e| e.index_status == Some(DeltaStatus::Conflicted))
            .collect();
        let unstaged: Vec<&FileStatusEntry> = self
            .entries
            .iter()
            .filter(|e| {
                e.worktree_status.is_some() && e.worktree_status != Some(DeltaStatus::Conflicted)
            })
            .collect();

        if !conflicted.is_empty() {
            out.push_str("Unmerged paths:\n");
            for c in &conflicted {
                out.push_str(&format!("\tboth modified:   {}\n", c.path));
            }
            out.push('\n');
        }

        if !staged.is_empty() {
            out.push_str("Changes to be committed:\n");
            for s in &staged {
                let label = match s.index_status.unwrap() {
                    DeltaStatus::Added => "new file:",
                    DeltaStatus::Modified => "modified:",
                    DeltaStatus::Deleted => "deleted:",
                    DeltaStatus::Conflicted => "unmerged:",
                };
                out.push_str(&format!("\t{label:12} {}\n", s.path));
            }
            out.push('\n');
        }

        if !unstaged.is_empty() {
            out.push_str("Changes not staged for commit:\n");
            for u in &unstaged {
                let label = match u.worktree_status.unwrap() {
                    DeltaStatus::Modified => "modified:",
                    DeltaStatus::Deleted => "deleted:",
                    _ => "modified:",
                };
                out.push_str(&format!("\t{label:12} {}\n", u.path));
            }
            out.push('\n');
        }

        if !self.untracked.is_empty() {
            out.push_str("Untracked files:\n");
            for u in &self.untracked {
                out.push_str(&format!("\t{u}\n"));
            }
            out.push('\n');
        }

        if staged.is_empty() && unstaged.is_empty() && conflicted.is_empty() && self.untracked.is_empty() {
            out.push_str("nothing to commit, working tree clean\n");
        }

        out
    }
}

/// Computes the full repository status (`HEAD` vs `.git/index` vs Working Tree).
pub fn compute_status(repo: &Repository, opts: StatusOptions) -> Result<StatusReport, GirError> {
    let Some(worktree) = repo.worktree() else {
        return Err(GirError::InvalidArgument(
            "cannot compute working-tree status in a bare repository".to_string(),
        ));
    };

    let branch = repo
        .refs()
        .read_symbolic_ref("HEAD")
        .ok()
        .map(|s| s.strip_prefix("refs/heads/").unwrap_or(&s).to_string());
    let head_oid = repo.refs().resolve_to_oid("HEAD").ok();
    let upstream = compute_upstream_status(repo, branch.as_deref(), head_oid);

    // 1. Flatten `HEAD` tree into `path -> (FileMode, ObjectId)`
    let head_files = if let Some(ref h_oid) = head_oid {
        if let Ok(ParsedObject::Commit(commit)) = repo.odb().read_parsed(h_oid) {
            flatten_tree(repo.odb(), &commit.tree)?
        } else {
            BTreeMap::new()
        }
    } else {
        BTreeMap::new()
    };

    // 2. Load `.git/index`
    let index = GitIndex::read(repo.git_dir())?;
    let index_mtime = index.index_mtime_sec();

    let mut stage0_map: BTreeMap<String, &IndexEntry> = BTreeMap::new();
    let mut conflicted_paths: HashSet<String> = HashSet::new();
    for entry in index.entries() {
        if entry.stage() == 0 {
            stage0_map.insert(entry.path.clone(), entry);
        } else {
            conflicted_paths.insert(entry.path.clone());
        }
    }

    // 3. Compute Staged changes (`HEAD` vs `Index`)
    let mut status_map: BTreeMap<String, (Option<DeltaStatus>, Option<DeltaStatus>)> =
        BTreeMap::new();

    for c_path in &conflicted_paths {
        status_map.insert(
            c_path.clone(),
            (Some(DeltaStatus::Conflicted), Some(DeltaStatus::Conflicted)),
        );
    }

    for (path, &(head_mode, head_blob_oid)) in &head_files {
        if conflicted_paths.contains(path) {
            continue;
        }
        match stage0_map.get(path) {
            None => {
                status_map.insert(path.clone(), (Some(DeltaStatus::Deleted), None));
            }
            Some(idx_entry) => {
                if idx_entry.file_mode() != head_mode || idx_entry.oid != head_blob_oid {
                    status_map.insert(path.clone(), (Some(DeltaStatus::Modified), None));
                }
            }
        }
    }

    for (path, _) in &stage0_map {
        if !head_files.contains_key(path) && !conflicted_paths.contains(path) {
            status_map.insert(path.clone(), (Some(DeltaStatus::Added), None));
        }
    }

    // 4. Compute Unstaged changes (`Index` vs Working Tree) in parallel using `rayon`
    let stage0_vec: Vec<(&String, &&IndexEntry)> = stage0_map.iter().collect();
    let unstaged_deltas: Vec<(String, DeltaStatus)> = stage0_vec
        .par_iter()
        .filter_map(|&(path, &entry)| {
            let abs_path = worktree.join(path);
            let Ok(meta) = fs::symlink_metadata(&abs_path) else {
                return Some((path.clone(), DeltaStatus::Deleted));
            };
            if meta.file_type().is_dir() {
                return Some((path.clone(), DeltaStatus::Deleted));
            }
            if entry.matches_stat(&meta, index_mtime) {
                return None;
            }
            // Stat differed or Racy Git triggered: verify mode and content SHA-1
            let wt_mode = if meta.file_type().is_symlink() {
                FileMode::Symlink
            } else if (meta.mode() & 0o111) != 0 {
                FileMode::Executable
            } else {
                FileMode::Regular
            };
            if wt_mode != entry.file_mode() {
                return Some((path.clone(), DeltaStatus::Modified));
            }
            let data = if meta.file_type().is_symlink() {
                fs::read_link(&abs_path)
                    .ok()?
                    .to_string_lossy()
                    .into_owned()
                    .into_bytes()
            } else {
                fs::read(&abs_path).ok()?
            };
            let wt_oid = RawObject {
                kind: ObjectKind::Blob,
                data,
            }
            .id();
            if wt_oid != entry.oid {
                Some((path.clone(), DeltaStatus::Modified))
            } else {
                None
            }
        })
        .collect();

    for (path, wt_status) in unstaged_deltas {
        let entry = status_map.entry(path).or_insert((None, None));
        entry.1 = Some(wt_status);
    }

    let entries: Vec<FileStatusEntry> = status_map
        .into_iter()
        .map(|(path, (index_status, worktree_status))| FileStatusEntry {
            path,
            index_status,
            worktree_status,
        })
        .collect();

    // 5. Walk working directory with `IgnoreStack` to collect untracked and ignored paths
    let root_ignore = IgnoreStack::for_repository(repo);
    let mut untracked = Vec::new();
    let mut ignored = Vec::new();
    walk_untracked(
        worktree,
        "",
        &root_ignore,
        &index,
        opts,
        &mut untracked,
        &mut ignored,
    )?;

    untracked.sort();
    ignored.sort();

    Ok(StatusReport {
        branch,
        head_oid,
        upstream,
        entries,
        untracked,
        ignored,
    })
}

fn walk_untracked(
    worktree: &Path,
    rel_dir: &str,
    ignore_stack: &IgnoreStack,
    index: &GitIndex,
    opts: StatusOptions,
    untracked: &mut Vec<String>,
    ignored: &mut Vec<String>,
) -> Result<(), GirError> {
    let abs_dir = if rel_dir.is_empty() {
        worktree.to_path_buf()
    } else {
        worktree.join(rel_dir)
    };

    let mut dir_entries: Vec<_> = fs::read_dir(&abs_dir)?
        .filter_map(|e| e.ok())
        .collect();
    dir_entries.sort_by_key(|e| e.file_name());

    for entry in dir_entries {
        let fname = entry.file_name();
        let Some(name_str) = fname.to_str() else {
            continue;
        };
        if rel_dir.is_empty() && name_str == ".git" {
            continue;
        }

        let rel_path = if rel_dir.is_empty() {
            name_str.to_string()
        } else {
            format!("{rel_dir}/{name_str}")
        };

        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        let is_dir = meta.file_type().is_dir();

        if is_dir {
            let has_tracked = index.contains_path_or_dir(&rel_path);
            let is_ign = ignore_stack.is_ignored(&rel_path, true);
            if is_ign && !has_tracked {
                if opts.include_ignored {
                    ignored.push(format!("{rel_path}/"));
                }
                continue;
            }

            let child_ignore = ignore_stack.enter_dir(&rel_path, &path);

            // If directory has no tracked files and `-uall` is false, collapse into `<rel_path>/`
            // if it contains at least one non-ignored untracked file
            if !has_tracked && !opts.untracked_all {
                if dir_contains_untracked_file(&path, &rel_path, &child_ignore)? {
                    untracked.push(format!("{rel_path}/"));
                }
                if opts.include_ignored {
                    let mut dummy_untracked = Vec::new();
                    walk_untracked(
                        worktree,
                        &rel_path,
                        &child_ignore,
                        index,
                        opts,
                        &mut dummy_untracked,
                        ignored,
                    )?;
                }
            } else {
                walk_untracked(
                    worktree,
                    &rel_path,
                    &child_ignore,
                    index,
                    opts,
                    untracked,
                    ignored,
                )?;
            }
        } else {
            if index.contains_path_or_dir(&rel_path) {
                continue;
            }
            if ignore_stack.is_ignored(&rel_path, false) {
                if opts.include_ignored {
                    ignored.push(rel_path);
                }
            } else {
                untracked.push(rel_path);
            }
        }
    }

    Ok(())
}

fn dir_contains_untracked_file(
    abs_dir: &Path,
    rel_dir: &str,
    ignore_stack: &IgnoreStack,
) -> Result<bool, GirError> {
    for entry in fs::read_dir(abs_dir)? {
        let entry = entry?;
        let fname = entry.file_name();
        let Some(name_str) = fname.to_str() else {
            continue;
        };
        let rel_path = format!("{rel_dir}/{name_str}");
        let path = entry.path();
        let meta = fs::symlink_metadata(&path)?;
        if meta.file_type().is_dir() {
            if ignore_stack.is_ignored(&rel_path, true) {
                continue;
            }
            let child_ignore = ignore_stack.enter_dir(&rel_path, &path);
            if dir_contains_untracked_file(&path, &rel_path, &child_ignore)? {
                return Ok(true);
            }
        } else if !ignore_stack.is_ignored(&rel_path, false) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn compute_upstream_status(
    repo: &Repository,
    branch: Option<&str>,
    head_oid: Option<ObjectId>,
) -> Option<UpstreamStatus> {
    let branch_name = branch?;
    let local_oid = head_oid?;
    let cfg = repo.local_config().ok()?;

    let remote = cfg.get(&format!("branch.{branch_name}.remote"))?;
    let merge = cfg.get(&format!("branch.{branch_name}.merge"))?;
    let merge_short = merge.strip_prefix("refs/heads/").unwrap_or(merge);
    let upstream_ref = format!("refs/remotes/{remote}/{merge_short}");
    let upstream_oid = repo.refs().resolve_to_oid(&upstream_ref).ok()?;

    let (ahead, behind) = count_ahead_behind(repo, local_oid, upstream_oid);
    Some(UpstreamStatus {
        upstream_name: format!("{remote}/{merge_short}"),
        ahead,
        behind,
    })
}

fn count_ahead_behind(repo: &Repository, local: ObjectId, upstream: ObjectId) -> (usize, usize) {
    if local == upstream {
        return (0, 0);
    }
    let local_ancestors = collect_ancestors(repo, local);
    let upstream_ancestors = collect_ancestors(repo, upstream);

    let ahead = local_ancestors.difference(&upstream_ancestors).count();
    let behind = upstream_ancestors.difference(&local_ancestors).count();
    (ahead, behind)
}

fn collect_ancestors(repo: &Repository, start: ObjectId) -> HashSet<ObjectId> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();
    visited.insert(start);
    queue.push_back(start);

    while let Some(oid) = queue.pop_front() {
        if let Ok(ParsedObject::Commit(commit)) = repo.odb().read_parsed(&oid) {
            for parent in commit.parents {
                if visited.insert(parent) {
                    queue.push_back(parent);
                }
            }
        }
    }
    visited
}
