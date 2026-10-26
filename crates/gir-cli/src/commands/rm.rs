use std::env;
use std::fs;
use std::path::Path;

use clap::Args;
use gir_core::index::GitIndex;
use gir_core::reflog::snapshot::SafetySnapshotEngine;
use gir_core::{GirError, ObjectKind, RawObject, Repository};

use super::add::normalize_pathspecs;

#[derive(Debug, Args)]
pub struct RmArgs {
    /// Only remove from the index; keep working tree files untouched
    #[arg(long = "cached")]
    pub cached: bool,

    /// Override the up-to-date check
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Allow recursive removal when a leading directory name is given
    #[arg(short = 'r')]
    pub recursive: bool,

    /// Files or directories to remove
    #[arg(required = true)]
    pub pathspecs: Vec<String>,
}

pub fn run(args: RmArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let mut index = GitIndex::read(repo.git_dir())?;
    let rel_specs = normalize_pathspecs(&cwd, &worktree, &args.pathspecs);

    if !args.cached {
        // Capture pre-operation safety snapshot before unlinking working-tree files
        let _ = SafetySnapshotEngine::capture(&repo, "rm");
    }

    for spec in &rel_specs {
        let abs = worktree.join(spec);
        if abs.is_dir() && !args.recursive {
            return Err(GirError::InvalidArgument(format!(
                "not removing '{spec}' recursively without -r"
            )));
        }

        // Check for local unstaged modifications unless `--cached` or `--force` is set
        if !args.cached && !args.force {
            for entry in index.entries() {
                if entry.stage() == 0
                    && (entry.path == *spec || entry.path.starts_with(&format!("{spec}/")))
                {
                    let file_abs = worktree.join(&entry.path);
                    if let Ok(data) = fs::read(&file_abs) {
                        let wt_oid = RawObject {
                            kind: ObjectKind::Blob,
                            data,
                        }
                        .id();
                        if wt_oid != entry.oid {
                            return Err(GirError::InvalidArgument(format!(
                                "error: the following file has local modifications:\n    {}\n(use --cached to keep the file, or -f to force removal)",
                                entry.path
                            )));
                        }
                    }
                }
            }
        }

        let removed = index.remove_matching_prefix(spec);
        if removed.is_empty() {
            return Err(GirError::InvalidArgument(format!(
                "pathspec '{spec}' did not match any files"
            )));
        }

        for rel_path in &removed {
            println!("rm '{rel_path}'");
            if !args.cached {
                let target_file = worktree.join(rel_path);
                if target_file.exists() || fs::symlink_metadata(&target_file).is_ok() {
                    let _ = fs::remove_file(&target_file);
                }
            }
        }

        if !args.cached && abs.is_dir() && args.recursive {
            let _ = fs::remove_dir_all(&abs);
        }
    }

    index.write(repo.git_dir())
}
