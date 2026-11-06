use crate::app::{ActivePane, AppState};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

pub fn render_branches_pane(buf: &mut ScreenBuffer, rect: Rect, app: &AppState) {
    let focused = app.active_pane == ActivePane::Branches;
    buf.draw_box(
        rect,
        "[2] Branches, Tags & Stashes (Enter/b: Checkout)",
        focused,
    );
    let inner = rect.inner();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut cur_y = inner.y;
    for (idx, b) in app.branches.iter().enumerate() {
        if cur_y >= inner.y + inner.height {
            break;
        }
        let is_selected = idx == app.selected_branch_idx;
        let marker = if b.is_current { "* " } else { "  " };
        let line = format!(
            "{marker}{} ({}) {}",
            b.name,
            b.oid.short_hex(),
            b.subject
        );
        let style = if is_selected && focused {
            Style::selected()
        } else if b.is_current {
            Style::bold_fg(Color::Green)
        } else if b.is_remote {
            Style::fg(Color::Red)
        } else {
            Style::fg(Color::White)
        };
        buf.draw_text(inner.x + 1, cur_y, inner.width.saturating_sub(2), &line, style);
        cur_y += 1;
    }

    if !app.tags.is_empty() && cur_y + 1 < inner.y + inner.height {
        cur_y += 1;
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            "── Tags ──",
            Style::bold_fg(Color::Magenta),
        );
        cur_y += 1;
        for t in &app.tags {
            if cur_y >= inner.y + inner.height {
                break;
            }
            let line = format!("  tag: {t}");
            buf.draw_text(
                inner.x + 1,
                cur_y,
                inner.width.saturating_sub(2),
                &line,
                Style::fg(Color::Yellow),
            );
            cur_y += 1;
        }
    }

    if !app.stashes.is_empty() && cur_y + 1 < inner.y + inner.height {
        cur_y += 1;
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            "── Stashes ──",
            Style::bold_fg(Color::Cyan),
        );
        cur_y += 1;
        for s in &app.stashes {
            if cur_y >= inner.y + inner.height {
                break;
            }
            let line = format!("  stash@{{{}}}: {}", s.index, s.message);
            buf.draw_text(
                inner.x + 1,
                cur_y,
                inner.width.saturating_sub(2),
                &line,
                Style::fg(Color::Cyan),
            );
            cur_y += 1;
        }
    }
}
