use std::fs;

use gir_core::branch_ops::{list_branches, BranchEntry};
use gir_core::commit_ops::{create_commit, CommitOptions};
use gir_core::diff::{
    apply_hunks_to_blob, diff_index_to_worktree, diff_tree_to_index, FilePatch,
};
use gir_core::graph::{build_ref_decorations, peel_to_commit_object, AsciiGraphRenderer, RevWalk};
use gir_core::index::{GitIndex, IndexEntry};
use gir_core::merge::{merge_branch, MergeOptions};
use gir_core::reflog::snapshot::SafetySnapshotEngine;
use gir_core::reflog::{read_reflog, ReflogEntry};
use gir_core::stash::{stash_list, StashEntry};
use gir_core::status::{compute_status, DeltaStatus, StatusOptions, StatusReport};
use gir_core::tag_ops::list_tags;
use gir_core::worktree::{switch_or_checkout, SwitchOptions};
use gir_core::{GirError, ObjectKind, Repository};
use gir_transport::{fetch_from_remote, push_to_remote, PushOptions};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActivePane {
    Status,
    Branches,
    LogGraph,
    Snapshots,
    Diff,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputMode {
    Normal,
    CommitModal,
    UndoConfirmModal,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyEvent {
    Char(char),
    Enter,
    Esc,
    Backspace,
    Tab,
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Clone)]
pub struct StatusListItem {
    pub path: String,
    pub badge: String,
    pub is_staged: bool,
    pub is_untracked: bool,
}

#[derive(Debug, Clone)]
pub struct LogGraphItem {
    pub oid_short: String,
    pub subject: String,
    pub author: String,
    pub decorations: Vec<String>,
    pub graph_line: String,
}

/// Complete live state for the 5-pane `gir tui` workbench.
pub struct AppState {
    pub repo: Repository,
    pub active_pane: ActivePane,
    pub last_sidebar_pane: ActivePane,
    pub input_mode: InputMode,
    pub commit_msg_input: String,
    pub status_banner: String,

    pub status_report: StatusReport,
    pub status_items: Vec<StatusListItem>,
    pub selected_status_idx: usize,

    pub active_file_patch: Option<FilePatch>,
    pub selected_hunk_idx: usize,

    pub branches: Vec<BranchEntry>,
    pub tags: Vec<String>,
    pub stashes: Vec<StashEntry>,
    pub selected_branch_idx: usize,

    pub log_items: Vec<LogGraphItem>,
    pub selected_log_idx: usize,

    pub snapshot_entries: Vec<ReflogEntry>,
    pub reflog_entries: Vec<ReflogEntry>,
    pub selected_reflog_idx: usize,
}

impl AppState {
    pub fn new(repo: Repository) -> Result<Self, GirError> {
        let dummy_report = StatusReport {
            branch: None,
            head_oid: None,
            upstream: None,
            entries: Vec::new(),
            untracked: Vec::new(),
            ignored: Vec::new(),
        };
        let mut state = Self {
            repo,
            active_pane: ActivePane::Status,
            last_sidebar_pane: ActivePane::Status,
            input_mode: InputMode::Normal,
            commit_msg_input: String::new(),
            status_banner: "Ready — [1-4] Switch Pane | [Space] Stage/Unstage | [Tab] Hunk Diff | [c] Commit | [u] Undo | [q] Quit".to_string(),
            status_report: dummy_report,
            status_items: Vec::new(),
            selected_status_idx: 0,
            active_file_patch: None,
            selected_hunk_idx: 0,
            branches: Vec::new(),
            tags: Vec::new(),
            stashes: Vec::new(),
            selected_branch_idx: 0,
            log_items: Vec::new(),
            selected_log_idx: 0,
            snapshot_entries: Vec::new(),
            reflog_entries: Vec::new(),
            selected_reflog_idx: 0,
        };
        state.refresh()?;
        Ok(state)
    }

    /// Reloads status, diffs, branches, tags, stashes, DAG commit graph, and safety snapshots in-memory.
    pub fn refresh(&mut self) -> Result<(), GirError> {
        self.status_report = compute_status(&self.repo, StatusOptions::default())?;

        let mut items = Vec::new();
        for e in &self.status_report.entries {
            if let Some(st) = e.index_status {
                items.push(StatusListItem {
                    path: e.path.clone(),
                    badge: format!("S:{}", st.as_char()),
                    is_staged: true,
                    is_untracked: false,
                });
            }
            if let Some(wt) = e.worktree_status {
                if wt != DeltaStatus::Conflicted {
                    items.push(StatusListItem {
                        path: e.path.clone(),
                        badge: format!("W:{}", wt.as_char()),
                        is_staged: false,
                        is_untracked: false,
                    });
                }
            }
        }
        for u in &self.status_report.untracked {
            items.push(StatusListItem {
                path: u.trim_end_matches('/').to_string(),
                badge: "?? ".to_string(),
                is_staged: false,
                is_untracked: true,
            });
        }
        self.status_items = items;
        if self.selected_status_idx >= self.status_items.len() {
            self.selected_status_idx = self.status_items.len().saturating_sub(1);
        }

        self.refresh_selected_diff();

        self.branches = list_branches(&self.repo, true, false).unwrap_or_default();
        if self.selected_branch_idx >= self.branches.len() {
            self.selected_branch_idx = self.branches.len().saturating_sub(1);
        }
        self.tags = list_tags(&self.repo, None).unwrap_or_default();
        self.stashes = stash_list(&self.repo).unwrap_or_default();

        // Load DAG commit graph
        let mut log_items = Vec::new();
        if let Ok(walk) = RevWalk::from_revisions(&self.repo, &[], true) {
            let decorations = build_ref_decorations(&self.repo).unwrap_or_default();
            let mut graph_renderer = AsciiGraphRenderer::new();
            for item in walk.take(50) {
                let (graph_line, _, _) = graph_renderer.step(item.oid, &item.commit.parents);
                let decs = decorations.get(&item.oid).cloned().unwrap_or_default();
                log_items.push(LogGraphItem {
                    oid_short: item.oid.short_hex(),
                    subject: item.commit.subject().to_string(),
                    author: item.commit.author.name.clone(),
                    decorations: decs,
                    graph_line,
                });
            }
        }
        self.log_items = log_items;
        if self.selected_log_idx >= self.log_items.len() {
            self.selected_log_idx = self.log_items.len().saturating_sub(1);
        }

        self.snapshot_entries = SafetySnapshotEngine::list(&self.repo).unwrap_or_default();
        let mut rlog = read_reflog(self.repo.git_dir(), "HEAD").unwrap_or_default();
        rlog.reverse();
        self.reflog_entries = rlog;
        if self.selected_reflog_idx >= self.reflog_entries.len() {
            self.selected_reflog_idx = self.reflog_entries.len().saturating_sub(1);
        }

        Ok(())
    }

    pub fn refresh_selected_diff(&mut self) {
        self.active_file_patch = None;
        let Some(selected) = self.status_items.get(self.selected_status_idx) else {
            return;
        };
        let Ok(index) = GitIndex::read(self.repo.git_dir()) else {
            return;
        };
        let specs = vec![selected.path.clone()];

        if selected.is_staged {
            let head_tree = self
                .repo
                .refs()
                .resolve_to_oid("HEAD")
                .ok()
                .and_then(|oid| peel_to_commit_object(self.repo.odb(), &oid).ok())
                .map(|(_, c)| c.tree);
            if let Ok(mut patches) =
                diff_tree_to_index(self.repo.odb(), head_tree.as_ref(), &index, &specs)
            {
                self.active_file_patch = patches.pop();
            }
        } else if let Ok(mut patches) = diff_index_to_worktree(&self.repo, &index, &specs) {
            self.active_file_patch = patches.pop();
        }
        self.selected_hunk_idx = 0;
    }

    /// Processes a single `KeyEvent`, returning `Ok(true)` if the user requested to quit `gir tui`.
    pub fn handle_key(&mut self, key: KeyEvent) -> Result<bool, GirError> {
        match self.input_mode {
            InputMode::CommitModal => return self.handle_commit_modal_key(key),
            InputMode::UndoConfirmModal => return self.handle_undo_modal_key(key),
            InputMode::Normal => {}
        }

        match key {
            KeyEvent::Char('q') | KeyEvent::Esc => return Ok(true),
            KeyEvent::Char('1') => {
                self.active_pane = ActivePane::Status;
                self.last_sidebar_pane = ActivePane::Status;
            }
            KeyEvent::Char('2') => {
                self.active_pane = ActivePane::Branches;
                self.last_sidebar_pane = ActivePane::Branches;
            }
            KeyEvent::Char('3') => {
                self.active_pane = ActivePane::LogGraph;
                self.last_sidebar_pane = ActivePane::LogGraph;
            }
            KeyEvent::Char('4') => {
                self.active_pane = ActivePane::Snapshots;
                self.last_sidebar_pane = ActivePane::Snapshots;
            }
            KeyEvent::Tab => {
                if self.active_pane == ActivePane::Diff {
                    self.active_pane = self.last_sidebar_pane;
                } else {
                    self.last_sidebar_pane = self.active_pane;
                    self.active_pane = ActivePane::Diff;
                }
            }
            KeyEvent::Down | KeyEvent::Char('j') => self.move_selection_down(),
            KeyEvent::Up | KeyEvent::Char('k') => self.move_selection_up(),
            KeyEvent::Char(' ') | KeyEvent::Char('s') => {
                if self.active_pane == ActivePane::Diff {
                    self.stage_selected_hunk()?;
                } else if self.active_pane == ActivePane::Status {
                    self.toggle_stage_selected_file()?;
                }
            }
            KeyEvent::Char('c') => {
                self.input_mode = InputMode::CommitModal;
                self.commit_msg_input.clear();
            }
            KeyEvent::Char('u') => {
                self.input_mode = InputMode::UndoConfirmModal;
            }
            KeyEvent::Char('b') | KeyEvent::Enter => {
                if self.active_pane == ActivePane::Branches {
                    self.checkout_selected_branch()?;
                }
            }
            KeyEvent::Char('p') => {
                self.pull_origin()?;
            }
            KeyEvent::Char('P') => {
                self.push_origin()?;
            }
            KeyEvent::Char('r') => {
                self.refresh()?;
                self.status_banner = "Refreshed repository state.".to_string();
            }
            _ => {}
        }

        Ok(false)
    }

    fn move_selection_down(&mut self) {
        match self.active_pane {
            ActivePane::Status => {
                if self.selected_status_idx + 1 < self.status_items.len() {
                    self.selected_status_idx += 1;
                    self.refresh_selected_diff();
                }
            }
            ActivePane::Diff => {
                if let Some(ref patch) = self.active_file_patch {
                    if self.selected_hunk_idx + 1 < patch.hunks.len() {
                        self.selected_hunk_idx += 1;
                    }
                }
            }
            ActivePane::Branches => {
                if self.selected_branch_idx + 1 < self.branches.len() {
                    self.selected_branch_idx += 1;
                }
            }
            ActivePane::LogGraph => {
                if self.selected_log_idx + 1 < self.log_items.len() {
                    self.selected_log_idx += 1;
                }
            }
            ActivePane::Snapshots => {
                if self.selected_reflog_idx + 1 < self.reflog_entries.len() {
                    self.selected_reflog_idx += 1;
                }
            }
        }
    }

    fn move_selection_up(&mut self) {
        match self.active_pane {
            ActivePane::Status => {
                if self.selected_status_idx > 0 {
                    self.selected_status_idx -= 1;
                    self.refresh_selected_diff();
                }
            }
            ActivePane::Diff => {
                if self.selected_hunk_idx > 0 {
                    self.selected_hunk_idx -= 1;
                }
            }
            ActivePane::Branches => {
                if self.selected_branch_idx > 0 {
                    self.selected_branch_idx -= 1;
                }
            }
            ActivePane::LogGraph => {
                if self.selected_log_idx > 0 {
                    self.selected_log_idx -= 1;
                }
            }
            ActivePane::Snapshots => {
                if self.selected_reflog_idx > 0 {
                    self.selected_reflog_idx -= 1;
                }
            }
        }
    }

    fn toggle_stage_selected_file(&mut self) -> Result<(), GirError> {
        let Some(item) = self.status_items.get(self.selected_status_idx).cloned() else {
            return Ok(());
        };
        let Some(worktree) = self.repo.worktree().map(std::path::Path::to_path_buf) else {
            return Ok(());
        };

        if item.is_staged {
            gir_core::worktree::reset_paths(&self.repo, "HEAD", std::slice::from_ref(&item.path))?;
            self.status_banner = format!("Unstaged '{}'", item.path);
        } else {
            let mut index = GitIndex::read(self.repo.git_dir())?;
            let abs = worktree.join(&item.path);
            if let Ok(meta) = fs::symlink_metadata(&abs) {
                if meta.is_file() || meta.file_type().is_symlink() {
                    let bytes = gir_core::diff::read_worktree_entry(&abs, &meta)?;
                    let oid = self.repo.odb().write_raw(ObjectKind::Blob, &bytes)?;
                    index.add_entry(IndexEntry::from_metadata(&item.path, oid, 0, &meta));
                }
            } else {
                index.remove_path(&item.path);
            }
            index.write(self.repo.git_dir())?;
            self.status_banner = format!("Staged '{}'", item.path);
        }

        self.refresh()
    }

    fn stage_selected_hunk(&mut self) -> Result<(), GirError> {
        let is_staged_selection = self
            .status_items
            .get(self.selected_status_idx)
            .map(|i| i.is_staged)
            .unwrap_or(false);
        let Some(ref patch) = self.active_file_patch else {
            return Ok(());
        };
        if self.selected_hunk_idx >= patch.hunks.len() {
            return Ok(());
        }

        let base_bytes = match patch.old_oid {
            Some(oid) => self.repo.odb().read_raw(&oid)?.data,
            None => Vec::new(),
        };

        let hunks_to_apply: Vec<&_> = if is_staged_selection {
            // Unstaging the selected hunk from a staged diff means re-applying all OTHER hunks to `HEAD`
            patch
                .hunks
                .iter()
                .enumerate()
                .filter_map(|(idx, h)| (idx != self.selected_hunk_idx).then_some(h))
                .collect()
        } else {
            vec![&patch.hunks[self.selected_hunk_idx]]
        };

        let new_bytes = apply_hunks_to_blob(&base_bytes, &hunks_to_apply)?;
        let mut index = GitIndex::read(self.repo.git_dir())?;

        if is_staged_selection && hunks_to_apply.is_empty() && patch.old_oid.is_none() {
            // Unstaged the only hunk of a newly added file -> remove from index
            index.remove_path(&patch.path);
        } else {
            let new_oid = self.repo.odb().write_raw(ObjectKind::Blob, &new_bytes)?;
            let mode = patch
                .new_mode
                .or(patch.old_mode)
                .unwrap_or(gir_core::object::tree::FileMode::Regular);
            let mut entry = IndexEntry::from_tree_entry(patch.path.clone(), mode, new_oid, 0);
            entry.file_size = new_bytes.len() as u32;
            index.add_entry(entry);
        }
        index.write(self.repo.git_dir())?;

        let action = if is_staged_selection {
            "Unstaged"
        } else {
            "Staged"
        };
        self.status_banner = format!(
            "{action} hunk {}/{} of '{}'",
            self.selected_hunk_idx + 1,
            patch.hunks.len(),
            patch.path
        );
        self.refresh()
    }

    fn checkout_selected_branch(&mut self) -> Result<(), GirError> {
        let Some(b) = self.branches.get(self.selected_branch_idx).cloned() else {
            return Ok(());
        };
        if b.is_remote {
            return Ok(());
        }
        match switch_or_checkout(
            &self.repo,
            &SwitchOptions {
                target: Some(b.name.clone()),
                ..Default::default()
            },
        ) {
            Ok(_) => {
                self.status_banner = format!("Switched to branch '{}'", b.name);
            }
            Err(e) => {
                self.status_banner = format!("Checkout error: {e}");
            }
        }
        self.refresh()
    }

    fn pull_origin(&mut self) -> Result<(), GirError> {
        match fetch_from_remote(&self.repo, "origin", &[]) {
            Ok(_) => {
                if let Some(ref b) = self.status_report.branch {
                    let tracking = format!("origin/{b}");
                    let _ = merge_branch(
                        &self.repo,
                        &MergeOptions {
                            target: Some(tracking),
                            ..Default::default()
                        },
                    );
                }
                self.status_banner = "Pulled from origin.".to_string();
            }
            Err(e) => {
                self.status_banner = format!("Pull failed: {e}");
            }
        }
        self.refresh()
    }

    fn push_origin(&mut self) -> Result<(), GirError> {
        match push_to_remote(&self.repo, &PushOptions::default()) {
            Ok(_) => {
                self.status_banner = "Pushed to origin.".to_string();
            }
            Err(e) => {
                self.status_banner = format!("Push failed: {e}");
            }
        }
        self.refresh()
    }

    fn handle_commit_modal_key(&mut self, key: KeyEvent) -> Result<bool, GirError> {
        match key {
            KeyEvent::Esc => {
                self.input_mode = InputMode::Normal;
                self.status_banner = "Commit cancelled.".to_string();
            }
            KeyEvent::Backspace => {
                self.commit_msg_input.pop();
            }
            KeyEvent::Char(ch) => {
                self.commit_msg_input.push(ch);
            }
            KeyEvent::Enter => {
                let msg = self.commit_msg_input.trim().to_string();
                self.input_mode = InputMode::Normal;
                if msg.is_empty() {
                    self.status_banner = "Aborted: empty commit message.".to_string();
                    return Ok(false);
                }
                match create_commit(
                    &self.repo,
                    &CommitOptions {
                        message: msg,
                        ..Default::default()
                    },
                ) {
                    Ok(outcome) => {
                        self.status_banner = format!(
                            "Committed [{} {}] {}",
                            outcome.branch_display,
                            outcome.commit_oid.short_hex(),
                            outcome.subject
                        );
                    }
                    Err(e) => {
                        self.status_banner = format!("Commit error: {e}");
                    }
                }
                self.refresh()?;
            }
            _ => {}
        }
        Ok(false)
    }

    fn handle_undo_modal_key(&mut self, key: KeyEvent) -> Result<bool, GirError> {
        match key {
            KeyEvent::Char('y') | KeyEvent::Char('Y') | KeyEvent::Enter => {
                self.input_mode = InputMode::Normal;
                match SafetySnapshotEngine::restore(&self.repo, None) {
                    Ok(snap) => {
                        self.status_banner = format!(
                            "Restored safety snapshot '{}' (HEAD -> {})",
                            snap.operation,
                            snap.head_oid.short_hex()
                        );
                    }
                    Err(e) => {
                        self.status_banner = format!("Undo failed: {e}");
                    }
                }
                self.refresh()?;
            }
            _ => {
                self.input_mode = InputMode::Normal;
                self.status_banner = "Undo cancelled.".to_string();
            }
        }
        Ok(false)
    }
}
