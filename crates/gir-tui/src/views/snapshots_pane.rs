use crate::app::{ActivePane, AppState};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

pub fn render_snapshots_pane(buf: &mut ScreenBuffer, rect: Rect, app: &AppState) {
    let focused = app.active_pane == ActivePane::Snapshots;
    buf.draw_box(
        rect,
        "[4] Safety Snapshots & Reflog Time-Machine (u: Instant Undo)",
        focused,
    );
    let inner = rect.inner();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let mut cur_y = inner.y;
    buf.draw_text(
        inner.x + 1,
        cur_y,
        inner.width.saturating_sub(2),
        "── Pre-Operation Safety Snapshot (refs/gir/snapshots/latest) ──",
        Style::bold_fg(Color::Magenta),
    );
    cur_y += 1;

    if let Some(snap) = app.snapshot_entries.first() {
        let line = format!(
            "  [READY] {} ({})",
            snap.message,
            snap.new_oid.short_hex()
        );
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            &line,
            Style::bold_fg(Color::Green),
        );
    } else {
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            "  No safety snapshots recorded yet.",
            Style::fg(Color::DarkGray),
        );
    }
    cur_y += 2;

    if cur_y < inner.y + inner.height {
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            "── HEAD Reflog History ──",
            Style::bold_fg(Color::Cyan),
        );
        cur_y += 1;
    }

    for (idx, entry) in app.reflog_entries.iter().enumerate() {
        if cur_y >= inner.y + inner.height {
            break;
        }
        let is_selected = idx == app.selected_reflog_idx;
        let line = format!(
            "  {} HEAD@{{{}}}: {}",
            entry.new_oid.short_hex(),
            idx,
            entry.message
        );
        let style = if is_selected && focused {
            Style::selected()
        } else {
            Style::fg(Color::White)
        };
        buf.draw_text(inner.x + 1, cur_y, inner.width.saturating_sub(2), &line, style);
        cur_y += 1;
    }
}
