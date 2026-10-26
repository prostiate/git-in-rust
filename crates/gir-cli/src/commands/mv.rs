use std::env;
use std::fs;
use std::path::Path;

use clap::Args;
use gir_core::index::GitIndex;
use gir_core::{GirError, Repository};

use super::add::{normalize_pathspecs, stage_single_file};

#[derive(Debug, Args)]
pub struct MvArgs {
    /// Force renaming or moving of file even if target exists
    #[arg(short = 'f', long = "force")]
    pub force: bool,

    /// Source file or directory
    pub source: String,

    /// Destination file or directory
    pub destination: String,
}

pub fn run(args: MvArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let Some(worktree) = repo.worktree().map(Path::to_path_buf) else {
        return Err(GirError::InvalidArgument(
            "this operation must be run in a work tree".to_string(),
        ));
    };

    let mut index = GitIndex::read(repo.git_dir())?;
    let norm = normalize_pathspecs(&cwd, &worktree, &[args.source.clone(), args.destination.clone()]);
    let src_rel = &norm[0];
    let mut dst_rel = norm[1].clone();

    let src_abs = worktree.join(src_rel);
    let mut dst_abs = worktree.join(&dst_rel);

    if !src_abs.exists() && fs::symlink_metadata(&src_abs).is_err() {
        return Err(GirError::InvalidArgument(format!(
            "bad source, source={}, destination={}",
            args.source, args.destination
        )));
    }

    if !index.contains_path_or_dir(src_rel) {
        return Err(GirError::InvalidArgument(format!(
            "not under version control, source={src_rel}"
        )));
    }

    // If destination is an existing directory, move source inside that directory
    if dst_abs.is_dir() {
        if let Some(fname) = src_abs.file_name().and_then(|s| s.to_str()) {
            dst_rel = if dst_rel.is_empty() {
                fname.to_string()
            } else {
                format!("{dst_rel}/{fname}")
            };
            dst_abs = worktree.join(&dst_rel);
        }
    }

    if dst_abs.exists() && !args.force {
        return Err(GirError::InvalidArgument(format!(
            "destination exists, source={}, destination={}",
            src_rel, dst_rel
        )));
    }

    if let Some(parent) = dst_abs.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::rename(&src_abs, &dst_abs)?;

    // Update index entries from `src_rel` to `dst_rel`
    let removed = index.remove_matching_prefix(src_rel);

    for old_path in removed {
        let suffix = old_path.strip_prefix(src_rel).unwrap_or("");
        let new_path = format!("{dst_rel}{suffix}");
        let new_abs = worktree.join(&new_path);
        let meta = fs::symlink_metadata(&new_abs)?;
        stage_single_file(&repo, &new_abs, &new_path, &meta, &mut index)?;
    }

    index.write(repo.git_dir())
}
