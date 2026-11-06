use std::fs;
use std::path::Path;

use crate::error::GirError;
use crate::graph::peel_to_commit_object;
use crate::hash::ObjectId;
use crate::index::{write_index_to_tree, GitIndex};
use crate::merge::{
    apply_tree_merge_to_worktree, merge_trees_3way, read_oid_file, write_state_file,
    ConflictStyle, MergeLabels, MergedPathState,
};
use crate::object::commit::Commit;
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::worktree::{checkout_tree, fallback_sig};

#[derive(Debug, Clone, Default)]
pub struct CherryPickOptions {
    pub commits: Vec<String>,
    pub no_commit: bool,
    pub cont: bool,
    pub abort: bool,
    pub skip: bool,
}

#[derive(Debug, Clone)]
pub enum CherryPickStatus {
    Applied {
        new_commits: Vec<ObjectId>,
    },
    Conflicted {
        commit_oid: ObjectId,
        conflicts: Vec<String>,
        descriptions: Vec<String>,
    },
    Aborted,
}

/// Applies one or more commits onto `HEAD` via 3-way merge (`gir cherry-pick`).
pub fn cherry_pick(
    repo: &Repository,
    opts: &CherryPickOptions,
) -> Result<CherryPickStatus, GirError> {
    if opts.abort {
        return cherry_pick_abort(repo);
    }
    if opts.skip {
        return cherry_pick_skip(repo);
    }
    if opts.cont {
        return cherry_pick_continue(repo);
    }

    if opts.commits.is_empty() {
        return Err(GirError::InvalidArgument(
            "you must specify commit(s) to cherry-pick".to_string(),
        ));
    }

    let head_oid = repo.refs().resolve_to_oid("HEAD")?;
    let (orig_head_oid, _) = peel_to_commit_object(repo.odb(), &head_oid)?;

    let mut queue = Vec::with_capacity(opts.commits.len());
    for spec in &opts.commits {
        let raw = parse_revision(repo, spec)?;
        let (c_oid, _) = peel_to_commit_object(repo.odb(), &raw)?;
        queue.push(c_oid);
    }

    let _ = SafetySnapshotEngine::capture(repo, "cherry-pick");
    write_state_file(repo.git_dir(), "ORIG_HEAD", &format!("{orig_head_oid}\n"))?;

    if queue.len() > 1 {
        save_sequencer_state(repo.git_dir(), orig_head_oid, &queue)?;
    }

    apply_cherry_pick_queue(repo, orig_head_oid, &queue, opts.no_commit)
}

fn apply_cherry_pick_queue(
    repo: &Repository,
    orig_head_oid: ObjectId,
    queue: &[ObjectId],
    no_commit: bool,
) -> Result<CherryPickStatus, GirError> {
    let mut new_commits = Vec::new();

    for (idx, &pick_oid) in queue.iter().enumerate() {
        let (_, pick_commit) = peel_to_commit_object(repo.odb(), &pick_oid)?;
        let head_oid = repo.refs().resolve_to_oid("HEAD")?;
        let (ours_oid, ours_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;

        let base_tree = if let Some(parent_oid) = pick_commit.parents.first() {
            let (_, p_commit) = peel_to_commit_object(repo.odb(), parent_oid)?;
            Some(p_commit.tree)
        } else {
            None
        };

        let pick_short = pick_oid.short_hex();
        let pick_subj = pick_commit.subject();
        let base_label = format!("parent of {pick_short} ({pick_subj})");
        let theirs_label = format!("{pick_short} ({pick_subj})");

        let outcome = merge_trees_3way(
            repo.odb(),
            base_tree.as_ref(),
            &ours_commit.tree,
            &pick_commit.tree,
            MergeLabels {
                ours: "HEAD",
                base: &base_label,
                theirs: &theirs_label,
            },
            ConflictStyle::Merge,
        )?;

        apply_tree_merge_to_worktree(repo, &ours_commit.tree, &outcome)?;

        if outcome.has_conflicts() {
            write_state_file(repo.git_dir(), "CHERRY_PICK_HEAD", &format!("{pick_oid}\n"))?;
            write_state_file(repo.git_dir(), "MERGE_MSG", &pick_commit.message)?;
            save_sequencer_state(repo.git_dir(), orig_head_oid, &queue[idx + 1..])?;

            let mut descriptions = Vec::new();
            for state in outcome.paths.values() {
                if let MergedPathState::Conflict { description, .. } = state {
                    descriptions.push(description.clone());
                }
            }
            return Ok(CherryPickStatus::Conflicted {
                commit_oid: pick_oid,
                conflicts: outcome.conflicts,
                descriptions,
            });
        }

        if !no_commit {
            let new_oid = commit_picked_index(
                repo,
                ours_oid,
                &pick_commit.author,
                &pick_commit.message,
                "cherry-pick",
            )?;
            new_commits.push(new_oid);
        }
    }

    clear_sequencer_state(repo.git_dir());
    Ok(CherryPickStatus::Applied { new_commits })
}

pub(crate) fn commit_picked_index(
    repo: &Repository,
    parent_oid: ObjectId,
    author: &crate::object::commit::Signature,
    message: &str,
    reflog_prefix: &str,
) -> Result<ObjectId, GirError> {
    let mut index = GitIndex::read(repo.git_dir())?;
    if index.has_conflicts() {
        return Err(GirError::InvalidArgument(
            "Committing is not possible because you have unmerged files.".to_string(),
        ));
    }
    let tree_oid = write_index_to_tree(&mut index, repo.odb())?;
    index.write(repo.git_dir())?;

    let committer = fallback_sig(repo);
    let msg = if message.ends_with('\n') {
        message.to_string()
    } else {
        format!("{message}\n")
    };
    let new_commit = Commit {
        tree: tree_oid,
        parents: vec![parent_oid],
        author: author.clone(),
        committer: committer.clone(),
        message: msg,
    };
    let subject = new_commit.subject().to_string();
    let new_oid = repo.odb().write_object(&new_commit.to_raw())?;

    repo.refs().update_ref(
        "HEAD",
        new_oid,
        Some(parent_oid),
        false,
        &committer,
        &format!("{reflog_prefix}: {subject}"),
    )?;
    Ok(new_oid)
}

fn cherry_pick_continue(repo: &Repository) -> Result<CherryPickStatus, GirError> {
    let cp_head_path = repo.git_dir().join("CHERRY_PICK_HEAD");
    if !cp_head_path.is_file() {
        return Err(GirError::InvalidArgument(
            "error: no cherry-pick or revert in progress".to_string(),
        ));
    }

    let pick_oid = read_oid_file(&cp_head_path)?;
    let (_, pick_commit) = peel_to_commit_object(repo.odb(), &pick_oid)?;
    let msg = fs::read_to_string(repo.git_dir().join("MERGE_MSG"))
        .unwrap_or_else(|_| pick_commit.message.clone());

    let head_oid = repo.refs().resolve_to_oid("HEAD")?;
    let (ours_oid, _) = peel_to_commit_object(repo.odb(), &head_oid)?;

    let new_oid = commit_picked_index(repo, ours_oid, &pick_commit.author, &msg, "cherry-pick")?;
    let _ = fs::remove_file(&cp_head_path);
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));

    let (orig_head, remaining) = load_sequencer_state(repo.git_dir()).unwrap_or((ours_oid, Vec::new()));
    if remaining.is_empty() {
        clear_sequencer_state(repo.git_dir());
        return Ok(CherryPickStatus::Applied {
            new_commits: vec![new_oid],
        });
    }

    let mut status = apply_cherry_pick_queue(repo, orig_head, &remaining, false)?;
    if let CherryPickStatus::Applied { ref mut new_commits } = status {
        new_commits.insert(0, new_oid);
    }
    Ok(status)
}

fn cherry_pick_skip(repo: &Repository) -> Result<CherryPickStatus, GirError> {
    let cp_head_path = repo.git_dir().join("CHERRY_PICK_HEAD");
    if !cp_head_path.is_file() {
        return Err(GirError::InvalidArgument(
            "error: no cherry-pick in progress".to_string(),
        ));
    }

    let head_oid = repo.refs().resolve_to_oid("HEAD")?;
    let (ours_oid, ours_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;
    checkout_tree(repo, &ours_commit.tree, true, "cherry-pick --skip")?;

    let _ = fs::remove_file(&cp_head_path);
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));

    let (orig_head, remaining) = load_sequencer_state(repo.git_dir()).unwrap_or((ours_oid, Vec::new()));
    if remaining.is_empty() {
        clear_sequencer_state(repo.git_dir());
        return Ok(CherryPickStatus::Applied {
            new_commits: Vec::new(),
        });
    }

    apply_cherry_pick_queue(repo, orig_head, &remaining, false)
}

fn cherry_pick_abort(repo: &Repository) -> Result<CherryPickStatus, GirError> {
    let cp_head_path = repo.git_dir().join("CHERRY_PICK_HEAD");
    let seq_dir = repo.git_dir().join("sequencer");
    if !cp_head_path.is_file() && !seq_dir.is_dir() {
        return Err(GirError::InvalidArgument(
            "error: no cherry-pick or revert in progress".to_string(),
        ));
    }

    let orig_oid = load_sequencer_state(repo.git_dir())
        .map(|(h, _)| h)
        .or_else(|_| read_oid_file(&repo.git_dir().join("ORIG_HEAD")))
        .or_else(|_| repo.refs().resolve_to_oid("HEAD"))?;
    let (commit_oid, orig_commit) = peel_to_commit_object(repo.odb(), &orig_oid)?;

    checkout_tree(repo, &orig_commit.tree, true, "cherry-pick --abort")?;
    let sig = fallback_sig(repo);
    repo.refs().update_ref(
        "HEAD",
        commit_oid,
        None,
        false,
        &sig,
        "cherry-pick --abort: updating HEAD",
    )?;

    let _ = fs::remove_file(&cp_head_path);
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    clear_sequencer_state(repo.git_dir());

    Ok(CherryPickStatus::Aborted)
}

fn save_sequencer_state(
    git_dir: &Path,
    orig_head: ObjectId,
    remaining: &[ObjectId],
) -> Result<(), GirError> {
    let seq_dir = git_dir.join("sequencer");
    fs::create_dir_all(&seq_dir)?;
    write_state_file(&seq_dir, "head", &format!("{orig_head}\n"))?;
    let mut todo = String::new();
    for oid in remaining {
        todo.push_str(&format!("pick {oid}\n"));
    }
    write_state_file(&seq_dir, "todo", &todo)
}

fn load_sequencer_state(git_dir: &Path) -> Result<(ObjectId, Vec<ObjectId>), GirError> {
    let seq_dir = git_dir.join("sequencer");
    let orig_head = read_oid_file(&seq_dir.join("head"))?;
    let todo_str = fs::read_to_string(seq_dir.join("todo")).unwrap_or_default();
    let mut remaining = Vec::new();
    for line in todo_str.lines() {
        if let Some(hex) = line.strip_prefix("pick ").map(str::trim) {
            if let Ok(oid) = ObjectId::from_hex(hex) {
                remaining.push(oid);
            }
        }
    }
    Ok((orig_head, remaining))
}

fn clear_sequencer_state(git_dir: &Path) {
    let _ = fs::remove_dir_all(git_dir.join("sequencer"));
}
