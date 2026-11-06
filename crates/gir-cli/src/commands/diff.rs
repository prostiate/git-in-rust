use std::env;

use clap::Args;
use gir_core::commit_ops::FileChangeKind;
use gir_core::diff::{
    diff_index_to_worktree, diff_tree_to_index, diff_tree_to_tree, diff_tree_to_worktree,
    summarize_patches, FilePatch,
};
use gir_core::graph::peel_to_commit_object;
use gir_core::index::GitIndex;
use gir_core::object::ParsedObject;
use gir_core::revision::{parse_revision, parse_revision_spec, RevisionSpec};
use gir_core::{GirError, ObjectId, Repository};

#[derive(Debug, Args)]
pub struct DiffArgs {
    /// View the changes you staged for the next commit relative to the named `<commit>` (default `HEAD`)
    #[arg(long = "staged", visible_alias = "cached")]
    pub staged: bool,

    /// Generate a diffstat
    #[arg(long = "stat")]
    pub stat: bool,

    /// Output only the last line of the `--stat` format containing total number of modified files, insertions, and deletions
    #[arg(long = "shortstat")]
    pub shortstat: bool,

    /// Show only names of changed files
    #[arg(long = "name-only")]
    pub name_only: bool,

    /// Show only names and status of changed files
    #[arg(long = "name-status")]
    pub name_status: bool,

    /// Revisions and/or paths (`[<rev1> [<rev2>]] [-- <paths>...]`)
    pub args: Vec<String>,
}

pub fn run(args: DiffArgs) -> Result<(), GirError> {
    let cwd = env::current_dir()?;
    let repo = Repository::discover(&cwd)?;
    let index = GitIndex::read(repo.git_dir())?;

    // Partition `args.args` into revision specs vs file pathspecs
    let mut revs: Vec<String> = Vec::new();
    let mut pathspecs: Vec<String> = Vec::new();

    for arg in &args.args {
        if !pathspecs.is_empty() {
            pathspecs.push(arg.clone());
            continue;
        }
        if revs.len() < 2
            && (arg.contains("..") || parse_revision(&repo, arg).is_ok())
        {
            revs.push(arg.clone());
        } else {
            pathspecs.push(arg.clone());
        }
    }

    if !pathspecs.is_empty() {
        if let Some(wt) = repo.worktree() {
            pathspecs = super::add::normalize_pathspecs(&cwd, wt, &pathspecs);
        }
    }

    let patches = match revs.as_slice() {
        [] => {
            if args.staged {
                let head_tree = resolve_head_tree_opt(&repo)?;
                diff_tree_to_index(repo.odb(), head_tree.as_ref(), &index, &pathspecs)?
            } else {
                diff_index_to_worktree(&repo, &index, &pathspecs)?
            }
        }
        [single] => {
            if single.contains("..") {
                match parse_revision_spec(&repo, single)? {
                    RevisionSpec::Range { from, to }
                    | RevisionSpec::SymmetricRange {
                        left: from,
                        right: to,
                    } => {
                        let t1 = peel_to_tree(&repo, &from)?;
                        let t2 = peel_to_tree(&repo, &to)?;
                        diff_tree_to_tree(repo.odb(), Some(&t1), &t2, &pathspecs)?
                    }
                    RevisionSpec::Single(oid) => {
                        let t = peel_to_tree(&repo, &oid)?;
                        diff_tree_to_worktree(&repo, &t, &index, &pathspecs)?
                    }
                }
            } else {
                let oid = parse_revision(&repo, single)?;
                let tree_oid = peel_to_tree(&repo, &oid)?;
                if args.staged {
                    diff_tree_to_index(repo.odb(), Some(&tree_oid), &index, &pathspecs)?
                } else {
                    diff_tree_to_worktree(&repo, &tree_oid, &index, &pathspecs)?
                }
            }
        }
        [rev1, rev2, ..] => {
            let oid1 = parse_revision(&repo, rev1)?;
            let oid2 = parse_revision(&repo, rev2)?;
            let t1 = peel_to_tree(&repo, &oid1)?;
            let t2 = peel_to_tree(&repo, &oid2)?;
            diff_tree_to_tree(repo.odb(), Some(&t1), &t2, &pathspecs)?
        }
    };

    render_patches(&patches, &args);
    Ok(())
}

fn render_patches(patches: &[FilePatch], args: &DiffArgs) {
    if patches.is_empty() {
        return;
    }

    if args.name_only {
        for p in patches {
            println!("{}", p.path);
        }
        return;
    }

    if args.name_status {
        for p in patches {
            let code = match p.kind {
                FileChangeKind::Added => 'A',
                FileChangeKind::Modified => 'M',
                FileChangeKind::Deleted => 'D',
            };
            println!("{code}\t{}", p.path);
        }
        return;
    }

    if args.shortstat {
        let summary = summarize_patches(patches);
        println!("{}", summary.format_shortstat());
        return;
    }

    if args.stat {
        let summary = summarize_patches(patches);
        print!("{}", summary.format_stat_block());
        return;
    }

    for p in patches {
        print!("{}", p.format_unified());
    }
}

fn resolve_head_tree_opt(repo: &Repository) -> Result<Option<ObjectId>, GirError> {
    let Ok(head_oid) = repo.refs().resolve_to_oid("HEAD") else {
        return Ok(None);
    };
    Ok(Some(peel_to_tree(repo, &head_oid)?))
}

fn peel_to_tree(repo: &Repository, oid: &ObjectId) -> Result<ObjectId, GirError> {
    match repo.odb().read_parsed(oid)? {
        ParsedObject::Tree(_) => Ok(*oid),
        ParsedObject::Commit(c) => Ok(c.tree),
        ParsedObject::Tag(t) => {
            let (_, c) = peel_to_commit_object(repo.odb(), &t.object)?;
            Ok(c.tree)
        }
        other => Err(GirError::UnexpectedObjectType {
            expected: "tree-ish".to_string(),
            found: other.kind().to_string(),
        }),
    }
}
