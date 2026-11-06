pub mod cherry_pick;
pub mod interactive;

use std::env;
use std::fs;
use std::io::Write;
use std::path::Path;
use std::process::Command;

use crate::error::GirError;
use crate::graph::{peel_to_commit_object, reachable_commits, RevWalk};
use crate::hash::ObjectId;
use crate::index::{write_index_to_tree, GitIndex};
use crate::merge::{
    apply_tree_merge_to_worktree, merge_trees_3way, read_oid_file, write_state_file,
    ConflictStyle, MergeLabels, MergedPathState,
};
use crate::object::commit::Commit;
use crate::reflog::append_reflog;
use crate::reflog::snapshot::SafetySnapshotEngine;
use crate::refs::RefTarget;
use crate::repository::Repository;
use crate::revision::parse_revision;
use crate::storage::atomic::LockFile;
use crate::worktree::{checkout_tree, fallback_sig};

pub use cherry_pick::{cherry_pick, CherryPickOptions, CherryPickStatus};
pub use interactive::{format_todo_script, parse_todo_script, TodoAction, TodoItem};

use cherry_pick::commit_picked_index;

#[derive(Debug, Clone, Default)]
pub struct RebaseOptions {
    pub upstream: Option<String>,
    pub onto: Option<String>,
    pub interactive: bool,
    pub cont: bool,
    pub abort: bool,
    pub skip: bool,
}

#[derive(Debug, Clone)]
pub enum RebaseStatus {
    UpToDate,
    Completed {
        new_head: ObjectId,
        branch_name: String,
    },
    Paused {
        commit_oid: ObjectId,
        reason: String,
        conflicts: Vec<String>,
        descriptions: Vec<String>,
    },
    Aborted,
}

/// Runs `gir rebase` (standard and interactive `-i`, plus `--continue`, `--skip`, and `--abort`)
/// using standard `.git/rebase-merge/` state files.
pub fn run_rebase(repo: &Repository, opts: &RebaseOptions) -> Result<RebaseStatus, GirError> {
    if opts.abort {
        return rebase_abort(repo);
    }
    if opts.skip {
        return rebase_skip(repo);
    }
    if opts.cont {
        return rebase_continue(repo);
    }

    let rb_dir = repo.git_dir().join("rebase-merge");
    if rb_dir.is_dir() {
        return Err(GirError::InvalidArgument(
            "It seems that there is already a rebase-merge directory, and\nI wonder if you are in the middle of another rebase.".to_string(),
        ));
    }

    let Some(ref upstream_spec) = opts.upstream else {
        return Err(GirError::InvalidArgument(
            "you must specify an upstream branch or commit to rebase onto".to_string(),
        ));
    };

    let head_raw = repo.refs().read_raw("HEAD")?;
    let head_name = match head_raw {
        Some(RefTarget::Symbolic(sym)) => sym,
        _ => "detached HEAD".to_string(),
    };

    let head_oid = repo.refs().resolve_to_oid("HEAD")?;
    let (orig_head_oid, _) = peel_to_commit_object(repo.odb(), &head_oid)?;

    let upstream_raw = parse_revision(repo, upstream_spec)?;
    let (upstream_oid, _) = peel_to_commit_object(repo.odb(), &upstream_raw)?;

    let onto_spec = opts.onto.as_deref().unwrap_or(upstream_spec);
    let onto_raw = parse_revision(repo, onto_spec)?;
    let (onto_oid, onto_commit) = peel_to_commit_object(repo.odb(), &onto_raw)?;

    // Collect commits in `upstream..orig_head` in oldest-first order
    let excluded = reachable_commits(repo.odb(), &[upstream_oid])?;
    let walk = RevWalk::build(repo.odb(), &[orig_head_oid], &excluded, true)?;
    let mut commits_oldest_first: Vec<_> = walk.collect();
    commits_oldest_first.reverse();

    if commits_oldest_first.is_empty() && onto_oid == orig_head_oid && !opts.interactive {
        return Ok(RebaseStatus::UpToDate);
    }

    let mut todo_items: Vec<TodoItem> = commits_oldest_first
        .into_iter()
        .map(|item| TodoItem {
            action: TodoAction::Pick,
            commit_oid: item.oid,
            subject: item.commit.subject().to_string(),
        })
        .collect();

    let _ = SafetySnapshotEngine::capture(repo, &format!("rebase {upstream_spec}"));

    // Initialize `.git/rebase-merge/` state directory
    fs::create_dir_all(&rb_dir)?;
    write_state_file(&rb_dir, "head-name", &format!("{head_name}\n"))?;
    write_state_file(&rb_dir, "orig-head", &format!("{orig_head_oid}\n"))?;
    write_state_file(&rb_dir, "onto", &format!("{onto_oid}\n"))?;
    write_state_file(&rb_dir, "done", "")?;

    let todo_path = rb_dir.join("git-rebase-todo");
    let initial_script = format_todo_script(&todo_items);
    write_state_file(&rb_dir, "git-rebase-todo", &initial_script)?;

    if opts.interactive {
        if let Some(editor_cmd) = resolve_sequence_editor(repo) {
            if editor_cmd != ":" && editor_cmd != "true" {
                let status = Command::new("sh")
                    .arg("-c")
                    .arg(format!("{editor_cmd} \"$1\""))
                    .arg("sh")
                    .arg(&todo_path)
                    .status()?;
                if !status.success() {
                    let _ = fs::remove_dir_all(&rb_dir);
                    return Err(GirError::InvalidArgument(
                        "Could not execute editor for interactive rebase".to_string(),
                    ));
                }
            }
        }
        let edited_script = fs::read_to_string(&todo_path)?;
        todo_items = parse_todo_script(repo, &edited_script)?;
        write_state_file(&rb_dir, "git-rebase-todo", &format_todo_script(&todo_items))?;
    }

    // Detach `HEAD` at `onto_oid` and transition worktree/index to `onto_commit.tree`
    if let Err(e) = checkout_tree(repo, &onto_commit.tree, false, "rebase checkout onto") {
        let _ = fs::remove_dir_all(&rb_dir);
        return Err(e);
    }
    detach_head_to(
        repo,
        orig_head_oid,
        onto_oid,
        &format!("rebase (start): checkout {onto_spec}"),
    )?;

    execute_rebase_todo_loop(repo)
}

fn execute_rebase_todo_loop(repo: &Repository) -> Result<RebaseStatus, GirError> {
    let rb_dir = repo.git_dir().join("rebase-merge");
    let todo_path = rb_dir.join("git-rebase-todo");
    let todo_text = fs::read_to_string(&todo_path).unwrap_or_default();
    let items = parse_todo_script(repo, &todo_text)?;

    for (idx, item) in items.iter().enumerate() {
        let remaining_script = format_todo_script(&items[idx + 1..]);
        let item_line = format!(
            "{} {} {}\n",
            item.action.as_str(),
            item.commit_oid.short_hex(),
            item.subject
        );

        match item.action {
            TodoAction::Drop => {
                append_done(&rb_dir, &item_line)?;
                write_state_file(&rb_dir, "git-rebase-todo", &remaining_script)?;
                continue;
            }
            TodoAction::Pick
            | TodoAction::Reword
            | TodoAction::Edit
            | TodoAction::Squash
            | TodoAction::Fixup => {
                let (_, pick_commit) = peel_to_commit_object(repo.odb(), &item.commit_oid)?;
                let head_oid = repo.refs().resolve_to_oid("HEAD")?;
                let (ours_oid, ours_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;

                let base_tree = if let Some(parent_oid) = pick_commit.parents.first() {
                    let (_, p_commit) = peel_to_commit_object(repo.odb(), parent_oid)?;
                    Some(p_commit.tree)
                } else {
                    None
                };

                let short = item.commit_oid.short_hex();
                let subj = pick_commit.subject();
                let base_label = format!("parent of {short} ({subj})");
                let theirs_label = format!("{short} ({subj})");

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

                append_done(&rb_dir, &item_line)?;
                write_state_file(&rb_dir, "git-rebase-todo", &remaining_script)?;

                if outcome.has_conflicts() {
                    write_state_file(
                        repo.git_dir(),
                        "REBASE_HEAD",
                        &format!("{}\n", item.commit_oid),
                    )?;
                    write_state_file(&rb_dir, "stopped-sha", &format!("{}\n", item.commit_oid))?;
                    write_state_file(&rb_dir, "current-action", item.action.as_str())?;
                    write_state_file(repo.git_dir(), "MERGE_MSG", &pick_commit.message)?;

                    let mut descriptions = Vec::new();
                    for state in outcome.paths.values() {
                        if let MergedPathState::Conflict { description, .. } = state {
                            descriptions.push(description.clone());
                        }
                    }
                    return Ok(RebaseStatus::Paused {
                        commit_oid: item.commit_oid,
                        reason: "conflict".to_string(),
                        conflicts: outcome.conflicts,
                        descriptions,
                    });
                }

                // Cleanly applied: commit or squash/fixup according to `item.action`
                apply_clean_rebase_action(
                    repo,
                    item.action,
                    ours_oid,
                    &ours_commit,
                    &pick_commit,
                )?;

                if item.action == TodoAction::Edit {
                    write_state_file(
                        repo.git_dir(),
                        "REBASE_HEAD",
                        &format!("{}\n", item.commit_oid),
                    )?;
                    write_state_file(&rb_dir, "stopped-sha", &format!("{}\n", item.commit_oid))?;
                    write_state_file(&rb_dir, "current-action", "edit")?;
                    return Ok(RebaseStatus::Paused {
                        commit_oid: item.commit_oid,
                        reason: "edit".to_string(),
                        conflicts: Vec::new(),
                        descriptions: Vec::new(),
                    });
                }
            }
        }
    }

    finalize_rebase(repo)
}

fn apply_clean_rebase_action(
    repo: &Repository,
    action: TodoAction,
    ours_oid: ObjectId,
    ours_commit: &Commit,
    pick_commit: &Commit,
) -> Result<ObjectId, GirError> {
    match action {
        TodoAction::Pick | TodoAction::Edit | TodoAction::Reword => {
            let mut msg = pick_commit.message.clone();
            if action == TodoAction::Reword {
                if let Some(editor_cmd) = resolve_commit_editor(repo) {
                    if editor_cmd != ":" && editor_cmd != "true" {
                        let edit_path = repo.git_dir().join("COMMIT_EDITMSG");
                        write_state_file(repo.git_dir(), "COMMIT_EDITMSG", &msg)?;
                        let status = Command::new("sh")
                            .arg("-c")
                            .arg(format!("{editor_cmd} \"$1\""))
                            .arg("sh")
                            .arg(&edit_path)
                            .status()?;
                        if status.success() {
                            if let Ok(updated) = fs::read_to_string(&edit_path) {
                                let cleaned = crate::commit_ops::normalize_commit_message(&updated);
                                if !cleaned.trim().is_empty() {
                                    msg = cleaned;
                                }
                            }
                        }
                    }
                }
            }
            commit_picked_index(repo, ours_oid, &pick_commit.author, &msg, "rebase (pick)")
        }
        TodoAction::Squash | TodoAction::Fixup => {
            let mut index = GitIndex::read(repo.git_dir())?;
            let tree_oid = write_index_to_tree(&mut index, repo.odb())?;
            index.write(repo.git_dir())?;

            let combined_msg = if action == TodoAction::Fixup {
                ours_commit.message.clone()
            } else {
                format!(
                    "{}\n{}",
                    ours_commit.message.trim_end(),
                    pick_commit.message
                )
            };

            let committer = fallback_sig(repo);
            let squashed = Commit {
                tree: tree_oid,
                parents: ours_commit.parents.clone(),
                author: ours_commit.author.clone(),
                committer: committer.clone(),
                message: combined_msg,
            };
            let subject = squashed.subject().to_string();
            let new_oid = repo.odb().write_object(&squashed.to_raw())?;
            repo.refs().update_ref(
                "HEAD",
                new_oid,
                Some(ours_oid),
                false,
                &committer,
                &format!("rebase ({}): {subject}", action.as_str()),
            )?;
            Ok(new_oid)
        }
        TodoAction::Drop => Ok(ours_oid),
    }
}

fn rebase_continue(repo: &Repository) -> Result<RebaseStatus, GirError> {
    let rb_dir = repo.git_dir().join("rebase-merge");
    if !rb_dir.is_dir() {
        return Err(GirError::InvalidArgument(
            "No rebase in progress?".to_string(),
        ));
    }

    let index = GitIndex::read(repo.git_dir())?;
    if index.has_conflicts() {
        return Err(GirError::InvalidArgument(
            "You must edit all merge conflicts and then\nmark them as resolved using gir add".to_string(),
        ));
    }

    let action_str = fs::read_to_string(rb_dir.join("current-action"))
        .unwrap_or_else(|_| "pick".to_string());
    let action = TodoAction::parse(action_str.trim()).unwrap_or(TodoAction::Pick);

    if action != TodoAction::Edit {
        if let Ok(stopped_oid) = read_oid_file(&rb_dir.join("stopped-sha")) {
            let (_, pick_commit) = peel_to_commit_object(repo.odb(), &stopped_oid)?;
            let head_oid = repo.refs().resolve_to_oid("HEAD")?;
            let (ours_oid, ours_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;

            let msg = fs::read_to_string(repo.git_dir().join("MERGE_MSG"))
                .unwrap_or_else(|_| pick_commit.message.clone());
            let mut custom_pick = pick_commit;
            custom_pick.message = msg;
            apply_clean_rebase_action(repo, action, ours_oid, &ours_commit, &custom_pick)?;
        }
    }

    let _ = fs::remove_file(repo.git_dir().join("REBASE_HEAD"));
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    let _ = fs::remove_file(rb_dir.join("stopped-sha"));
    let _ = fs::remove_file(rb_dir.join("current-action"));

    execute_rebase_todo_loop(repo)
}

fn rebase_skip(repo: &Repository) -> Result<RebaseStatus, GirError> {
    let rb_dir = repo.git_dir().join("rebase-merge");
    if !rb_dir.is_dir() {
        return Err(GirError::InvalidArgument(
            "No rebase in progress?".to_string(),
        ));
    }

    let head_oid = repo.refs().resolve_to_oid("HEAD")?;
    let (_, ours_commit) = peel_to_commit_object(repo.odb(), &head_oid)?;
    checkout_tree(repo, &ours_commit.tree, true, "rebase --skip")?;

    let _ = fs::remove_file(repo.git_dir().join("REBASE_HEAD"));
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    let _ = fs::remove_file(rb_dir.join("stopped-sha"));
    let _ = fs::remove_file(rb_dir.join("current-action"));

    execute_rebase_todo_loop(repo)
}

fn rebase_abort(repo: &Repository) -> Result<RebaseStatus, GirError> {
    let rb_dir = repo.git_dir().join("rebase-merge");
    if !rb_dir.is_dir() {
        return Err(GirError::InvalidArgument(
            "No rebase in progress?".to_string(),
        ));
    }

    let head_name = fs::read_to_string(rb_dir.join("head-name"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let orig_oid = read_oid_file(&rb_dir.join("orig-head"))?;
    let (_, orig_commit) = peel_to_commit_object(repo.odb(), &orig_oid)?;

    checkout_tree(repo, &orig_commit.tree, true, "rebase --abort")?;

    let sig = fallback_sig(repo);
    if head_name.starts_with("refs/heads/") {
        repo.refs()
            .update_ref(&head_name, orig_oid, None, false, &sig, "rebase (abort)")?;
        repo.refs().set_symbolic_ref(
            "HEAD",
            &head_name,
            Some(&sig),
            Some(&format!("rebase (abort): returning to {head_name}")),
        )?;
    } else {
        let cur_head = repo.refs().resolve_to_oid("HEAD").unwrap_or(ObjectId::ZERO);
        detach_head_to(repo, cur_head, orig_oid, "rebase (abort): updating HEAD")?;
    }

    let _ = fs::remove_file(repo.git_dir().join("REBASE_HEAD"));
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    let _ = fs::remove_dir_all(&rb_dir);

    Ok(RebaseStatus::Aborted)
}

fn finalize_rebase(repo: &Repository) -> Result<RebaseStatus, GirError> {
    let rb_dir = repo.git_dir().join("rebase-merge");
    let head_name = fs::read_to_string(rb_dir.join("head-name"))
        .unwrap_or_default()
        .trim()
        .to_string();
    let orig_oid = read_oid_file(&rb_dir.join("orig-head")).unwrap_or(ObjectId::ZERO);
    let new_head = repo.refs().resolve_to_oid("HEAD")?;

    write_state_file(repo.git_dir(), "ORIG_HEAD", &format!("{orig_oid}\n"))?;

    let sig = fallback_sig(repo);
    if head_name.starts_with("refs/heads/") {
        repo.refs().update_ref(
            &head_name,
            new_head,
            None,
            false,
            &sig,
            "rebase (finish): Coupled branch update",
        )?;
        repo.refs().set_symbolic_ref(
            "HEAD",
            &head_name,
            Some(&sig),
            Some(&format!("rebase (finish): returning to {head_name}")),
        )?;
    }

    let _ = fs::remove_file(repo.git_dir().join("REBASE_HEAD"));
    let _ = fs::remove_file(repo.git_dir().join("MERGE_MSG"));
    let _ = fs::remove_dir_all(&rb_dir);

    Ok(RebaseStatus::Completed {
        new_head,
        branch_name: head_name,
    })
}

fn detach_head_to(
    repo: &Repository,
    old_oid: ObjectId,
    new_oid: ObjectId,
    reflog_msg: &str,
) -> Result<(), GirError> {
    let head_path = repo.git_dir().join("HEAD");
    let mut lock = LockFile::acquire(&head_path)?;
    writeln!(lock, "{new_oid}")?;
    lock.commit()?;

    let sig = fallback_sig(repo);
    append_reflog(repo.git_dir(), "HEAD", old_oid, new_oid, &sig, reflog_msg)
}

fn append_done(rb_dir: &Path, line: &str) -> Result<(), GirError> {
    let done_path = rb_dir.join("done");
    let mut current = fs::read_to_string(&done_path).unwrap_or_default();
    current.push_str(line);
    write_state_file(rb_dir, "done", &current)
}

fn resolve_sequence_editor(repo: &Repository) -> Option<String> {
    if let Ok(val) = env::var("GIT_SEQUENCE_EDITOR") {
        if !val.trim().is_empty() {
            return Some(val);
        }
    }
    if let Ok(cfg) = repo.config() {
        if let Some(val) = cfg.get("sequence.editor") {
            if !val.trim().is_empty() {
                return Some(val.to_string());
            }
        }
    }
    resolve_commit_editor(repo)
}

fn resolve_commit_editor(repo: &Repository) -> Option<String> {
    if let Ok(val) = env::var("GIT_EDITOR") {
        if !val.trim().is_empty() {
            return Some(val);
        }
    }
    if let Ok(cfg) = repo.config() {
        if let Some(val) = cfg.get("core.editor") {
            if !val.trim().is_empty() {
                return Some(val.to_string());
            }
        }
    }
    if let Ok(val) = env::var("VISUAL") {
        if !val.trim().is_empty() {
            return Some(val);
        }
    }
    if let Ok(val) = env::var("EDITOR") {
        if !val.trim().is_empty() {
            return Some(val);
        }
    }
    None
}
