use crate::app::{ActivePane, AppState};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

pub fn render_log_graph_pane(buf: &mut ScreenBuffer, rect: Rect, app: &AppState) {
    let focused = app.active_pane == ActivePane::LogGraph;
    buf.draw_box(rect, "[3] Commit DAG Graph Log", focused);
    let inner = rect.inner();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    for (idx, item) in app.log_items.iter().enumerate() {
        let row = inner.y + (idx as u16);
        if row >= inner.y + inner.height {
            break;
        }
        let is_selected = idx == app.selected_log_idx;
        let dec = if item.decorations.is_empty() {
            String::new()
        } else {
            format!(" ({})", item.decorations.join(", "))
        };
        let line = format!(
            "{}{} {} — {} <{}>",
            item.graph_line, item.oid_short, dec, item.subject, item.author
        );
        let style = if is_selected && focused {
            Style::selected()
        } else {
            Style::fg(Color::Yellow)
        };
        buf.draw_text(inner.x + 1, row, inner.width.saturating_sub(2), &line, style);
    }
}
