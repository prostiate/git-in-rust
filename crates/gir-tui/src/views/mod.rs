pub mod branches_pane;
pub mod diff_pane;
pub mod log_graph_pane;
pub mod snapshots_pane;
pub mod status_pane;

use crate::app::{ActivePane, AppState, InputMode};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

use branches_pane::render_branches_pane;
use diff_pane::render_diff_pane;
use log_graph_pane::render_log_graph_pane;
use snapshots_pane::render_snapshots_pane;
use status_pane::render_status_pane;

/// Renders the entire 5-pane `gir tui` workbench into a `ScreenBuffer`.
pub fn render_app(buf: &mut ScreenBuffer, app: &AppState) {
    if buf.width < 20 || buf.height < 6 {
        return;
    }

    // 1. Top Header Bar
    let branch_label = app
        .status_report
        .branch
        .as_deref()
        .unwrap_or("detached HEAD");
    let head_short = app
        .status_report
        .head_oid
        .map(|id| id.short_hex())
        .unwrap_or_else(|| "unborn".to_string());
    let header_text = format!(
        " gir tui — Branch: {branch_label} ({head_short})  |  [1] Status  [2] Branches  [3] Log Graph  [4] Snapshots  [Tab] Diff "
    );
    buf.fill_rect(
        Rect::new(0, 0, buf.width, 1),
        ' ',
        Style {
            fg: Color::White,
            bg: Color::Blue,
            bold: true,
        },
    );
    buf.draw_text(
        0,
        0,
        buf.width,
        &header_text,
        Style {
            fg: Color::White,
            bg: Color::Blue,
            bold: true,
        },
    );

    // 2. Main Split Content Area (`y = 1 .. height - 1`)
    let body_height = buf.height.saturating_sub(2);
    let left_width = (buf.width * 45) / 100;
    let right_width = buf.width.saturating_sub(left_width);

    let left_rect = Rect::new(0, 1, left_width, body_height);
    let right_rect = Rect::new(left_width, 1, right_width, body_height);

    let sidebar_pane = if app.active_pane == ActivePane::Diff {
        app.last_sidebar_pane
    } else {
        app.active_pane
    };

    match sidebar_pane {
        ActivePane::Status | ActivePane::Diff => render_status_pane(buf, left_rect, app),
        ActivePane::Branches => render_branches_pane(buf, left_rect, app),
        ActivePane::LogGraph => render_log_graph_pane(buf, left_rect, app),
        ActivePane::Snapshots => render_snapshots_pane(buf, left_rect, app),
    }

    render_diff_pane(buf, right_rect, app);

    // 3. Modal Overlays (Commit Message Modal / Undo Confirmation Modal)
    match app.input_mode {
        InputMode::CommitModal => {
            let modal_w = buf.width.min(64).saturating_sub(4);
            let modal_h = 5;
            let mx = (buf.width.saturating_sub(modal_w)) / 2;
            let my = (buf.height.saturating_sub(modal_h)) / 2;
            let mrect = Rect::new(mx, my, modal_w, modal_h);
            buf.fill_rect(mrect, ' ', Style::default());
            buf.draw_box(mrect, "Commit Staged Changes (Enter: Commit, Esc: Cancel)", true);
            let prompt = format!("> {}_", app.commit_msg_input);
            buf.draw_text(
                mx + 2,
                my + 2,
                modal_w.saturating_sub(4),
                &prompt,
                Style::bold_fg(Color::Green),
            );
        }
        InputMode::UndoConfirmModal => {
            let modal_w = buf.width.min(70).saturating_sub(4);
            let modal_h = 5;
            let mx = (buf.width.saturating_sub(modal_w)) / 2;
            let my = (buf.height.saturating_sub(modal_h)) / 2;
            let mrect = Rect::new(mx, my, modal_w, modal_h);
            buf.fill_rect(mrect, ' ', Style::default());
            buf.draw_box(mrect, "Confirm Pre-Operation Safety Snapshot Restore", true);
            let desc = match app.snapshot_entries.first() {
                Some(s) => format!(
                    "Restore '{}' ({})? [y/Enter = Yes, n/Esc = Cancel]",
                    s.message,
                    s.new_oid.short_hex()
                ),
                None => "No safety snapshot found. Press Esc to close.".to_string(),
            };
            buf.draw_text(
                mx + 2,
                my + 2,
                modal_w.saturating_sub(4),
                &desc,
                Style::bold_fg(Color::Yellow),
            );
        }
        InputMode::Normal => {}
    }

    // 4. Bottom Status / Keybinding Bar
    let bottom_y = buf.height - 1;
    buf.fill_rect(
        Rect::new(0, bottom_y, buf.width, 1),
        ' ',
        Style {
            fg: Color::White,
            bg: Color::DarkGray,
            bold: false,
        },
    );
    buf.draw_text(
        1,
        bottom_y,
        buf.width.saturating_sub(2),
        &app.status_banner,
        Style {
            fg: Color::White,
            bg: Color::DarkGray,
            bold: false,
        },
    );
}
