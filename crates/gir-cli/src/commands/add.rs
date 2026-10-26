use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use clap::Args;
use gir_core::ignore::IgnoreStack;
use gir_core::index::{GitIndex, IndexEntry};
use gir_core::{GirError, ObjectKind, Repository};

#[derive(Debug, Args)]
pub struct AddArgs {
    /// Update the index not only where the working tree has a file matching <pathspec>
    /// but also where the index already has an entry (`-A` / `--all`)
    #[arg(short = 'A', long = "all")]
    pub all: bool,

    /// Update the index just where it already has an entry matching <pathspec> (`-u` / `--update`)
    #[arg(short = 'u', long = "update")]
    pub update: bool,

    /// Allow adding otherwise ignored files
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Files or directories to stage
    pub pathspecs: Vec<String>,
}

pub fn run(args: AddArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let mut index = GitIndex::read(repo.git_dir())?;
    let root_ignore = IgnoreStack::for_repository(&repo);

    if args.update {
        let prefixes = normalize_pathspecs(&cwd, &worktree, &args.pathspecs);
        stage_tracked_updates(&repo, &worktree, &mut index, &prefixes)?;
        index.write(repo.git_dir())?;
        return Ok(());
    }

    if args.all && args.pathspecs.is_empty() {
        stage_directory_recursive(&repo, &worktree, "", &root_ignore, &mut index, args.force)?;
        stage_tracked_updates(&repo, &worktree, &mut index, &[String::new()])?;
        index.write(repo.git_dir())?;
        return Ok(());
    }

    if args.pathspecs.is_empty() {
        return Err(GirError::InvalidArgument(
            "Nothing specified, nothing added.".to_string(),
        ));
    }

    let rel_specs = normalize_pathspecs(&cwd, &worktree, &args.pathspecs);
    for rel_spec in &rel_specs {
        let abs_target = if rel_spec.is_empty() {
            worktree.clone()
        } else {
            worktree.join(rel_spec)
        };

        if let Ok(meta) = fs::symlink_metadata(&abs_target) {
            if meta.file_type().is_dir() {
                let ignore = build_ignore_for_rel_dir(&worktree, &root_ignore, rel_spec);
                stage_directory_recursive(
                    &repo,
                    &worktree,
                    rel_spec,
                    &ignore,
                    &mut index,
                    args.force,
                )?;
                stage_tracked_updates(
                    &repo,
                    &worktree,
                    &mut index,
                    std::slice::from_ref(rel_spec),
                )?;
            } else {
                let parent_rel = Path::new(rel_spec)
                    .parent()
                    .and_then(Path::to_str)
                    .unwrap_or("");
                let ignore = build_ignore_for_rel_dir(&worktree, &root_ignore, parent_rel);
                let is_tracked = index.get(rel_spec, 0).is_some();
                if !args.force && !is_tracked && ignore.is_ignored(rel_spec, false) {
                    return Err(GirError::InvalidArgument(format!(
                        "The following paths are ignored by one of your .gitignore files:\n{rel_spec}\nhint: Use -f if you really want to add them."
                    )));
                }
                stage_single_file(&repo, &abs_target, rel_spec, &meta, &mut index)?;
            }
        } else if index.contains_path_or_dir(rel_spec) {
            // Path was deleted from working tree: stage the deletion
            index.remove_matching_prefix(rel_spec);
        } else {
            return Err(GirError::InvalidArgument(format!(
                "pathspec '{rel_spec}' did not match any files"
            )));
        }
    }

    index.write(repo.git_dir())
}

pub fn normalize_pathspecs(cwd: &Path, worktree: &Path, specs: &[String]) -> Vec<String> {
    if specs.is_empty() {
        return vec![String::new()];
    }
    let mut out = Vec::with_capacity(specs.len());
    for s in specs {
        let candidate = if Path::new(s).is_absolute() {
            PathBuf::from(s)
        } else {
            cwd.join(s)
        };
        let normalized = normalize_lexical(&candidate);
        let rel = normalized
            .strip_prefix(worktree)
            .unwrap_or(&normalized)
            .to_string_lossy()
            .replace('\\', "/");
        let clean = rel.trim_matches('/').to_string();
        if clean == "." {
            out.push(String::new());
        } else {
            out.push(clean);
        }
    }
    out
}

fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for comp in path.components() {
        match comp {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                out.pop();
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

fn build_ignore_for_rel_dir(
    worktree: &Path,
    root_ignore: &IgnoreStack,
    rel_dir: &str,
) -> IgnoreStack {
    let mut stack = root_ignore.clone();
    if rel_dir.is_empty() {
        return stack;
    }
    let mut acc = String::new();
    for part in rel_dir.split('/').filter(|p| !p.is_empty()) {
        if !acc.is_empty() {
            acc.push('/');
        }
        acc.push_str(part);
        let abs = worktree.join(&acc);
        stack = stack.enter_dir(&acc, &abs);
    }
    stack
}

fn stage_directory_recursive(
    repo: &Repository,
    worktree: &Path,
    rel_dir: &str,
    ignore_stack: &IgnoreStack,
    index: &mut GitIndex,
    force: bool,
) -> Result<(), GirError> {
    let abs_dir = if rel_dir.is_empty() {
        worktree.to_path_buf()
    } else {
        worktree.join(rel_dir)
    };

    let mut entries: Vec<_> = fs::read_dir(&abs_dir)?.filter_map(|e| e.ok()).collect();
    entries.sort_by_key(|e| e.file_name());

    for entry in entries {
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
        let abs_path = entry.path();
        let meta = fs::symlink_metadata(&abs_path)?;

        if meta.file_type().is_dir() {
            let has_tracked = index.contains_path_or_dir(&rel_path);
            if !force && !has_tracked && ignore_stack.is_ignored(&rel_path, true) {
                continue;
            }
            let child_ignore = ignore_stack.enter_dir(&rel_path, &abs_path);
            stage_directory_recursive(repo, worktree, &rel_path, &child_ignore, index, force)?;
        } else {
            let is_tracked = index.get(&rel_path, 0).is_some();
            if !force && !is_tracked && ignore_stack.is_ignored(&rel_path, false) {
                continue;
            }
            stage_single_file(repo, &abs_path, &rel_path, &meta, index)?;
        }
    }

    Ok(())
}

pub fn stage_single_file(
    repo: &Repository,
    abs_path: &Path,
    rel_path: &str,
    meta: &fs::Metadata,
    index: &mut GitIndex,
) -> Result<(), GirError> {
    let data = if meta.file_type().is_symlink() {
        fs::read_link(abs_path)?
            .to_string_lossy()
            .into_owned()
            .into_bytes()
    } else {
        fs::read(abs_path)?
    };
    let blob_oid = repo.odb().write_raw(ObjectKind::Blob, &data)?;
    let entry = IndexEntry::from_metadata(rel_path, blob_oid, 0, meta);
    index.add_entry(entry);
    Ok(())
}

fn stage_tracked_updates(
    repo: &Repository,
    worktree: &Path,
    index: &mut GitIndex,
    prefixes: &[String],
) -> Result<(), GirError> {
    let tracked_paths: Vec<String> = index
        .entries()
        .map(|e| e.path.clone())
        .filter(|p| {
            prefixes.iter().any(|pref| {
                pref.is_empty()
                    || p == pref
                    || p.starts_with(&format!("{pref}/"))
            })
        })
        .collect();

    for path in tracked_paths {
        let abs = worktree.join(&path);
        match fs::symlink_metadata(&abs) {
            Ok(meta) if !meta.file_type().is_dir() => {
                stage_single_file(repo, &abs, &path, &meta, index)?;
            }
            _ => {
                index.remove_path(&path);
            }
        }
    }
    Ok(())
}
