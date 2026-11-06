use crate::app::{ActivePane, AppState};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

pub fn render_status_pane(buf: &mut ScreenBuffer, rect: Rect, app: &AppState) {
    let focused = app.active_pane == ActivePane::Status;
    buf.draw_box(rect, "[1] Status & Files (Space: Stage/Unstage)", focused);
    let inner = rect.inner();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    if app.status_items.is_empty() {
        buf.draw_text(
            inner.x + 1,
            inner.y + 1,
            inner.width.saturating_sub(2),
            "Working tree clean — nothing to commit.",
            Style::fg(Color::Green),
        );
        return;
    }

    for (idx, item) in app.status_items.iter().enumerate() {
        let row = inner.y + (idx as u16);
        if row >= inner.y + inner.height {
            break;
        }
        let is_selected = idx == app.selected_status_idx;
        let line = format!("[{}] {}", item.badge, item.path);
        let style = if is_selected && focused {
            Style::selected()
        } else if item.is_staged {
            Style::fg(Color::Green)
        } else if item.is_untracked {
            Style::fg(Color::Cyan)
        } else {
            Style::fg(Color::Yellow)
        };
        buf.draw_text(inner.x + 1, row, inner.width.saturating_sub(2), &line, style);
    }
}
