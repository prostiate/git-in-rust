use gir_core::diff::HunkLine;

use crate::app::{ActivePane, AppState};
use crate::theme::{Color, Rect, ScreenBuffer, Style};

pub fn render_diff_pane(buf: &mut ScreenBuffer, rect: Rect, app: &AppState) {
    let focused = app.active_pane == ActivePane::Diff;
    buf.draw_box(
        rect,
        "[Tab] Unified Diff & Hunk Stage (s/Space: Stage Hunk)",
        focused,
    );
    let inner = rect.inner();
    if inner.height == 0 || inner.width == 0 {
        return;
    }

    let Some(ref patch) = app.active_file_patch else {
        buf.draw_text(
            inner.x + 1,
            inner.y + 1,
            inner.width.saturating_sub(2),
            "Select a modified file in [1] Status to inspect its diff hunks.",
            Style::fg(Color::DarkGray),
        );
        return;
    };

    let mut cur_y = inner.y;
    let hdr = format!("diff --git a/{0} b/{0}", patch.path);
    buf.draw_text(
        inner.x + 1,
        cur_y,
        inner.width.saturating_sub(2),
        &hdr,
        Style::bold_fg(Color::White),
    );
    cur_y += 1;

    for (h_idx, hunk) in patch.hunks.iter().enumerate() {
        if cur_y >= inner.y + inner.height {
            break;
        }
        let is_selected_hunk = h_idx == app.selected_hunk_idx;
        let hunk_hdr = format!(
            "{} @@ -{},{} +{},{} @@",
            if is_selected_hunk { "▶" } else { " " },
            hunk.old_start,
            hunk.old_count,
            hunk.new_start,
            hunk.new_count
        );
        let hdr_style = if is_selected_hunk && focused {
            Style::selected()
        } else {
            Style::bold_fg(Color::Cyan)
        };
        buf.draw_text(
            inner.x + 1,
            cur_y,
            inner.width.saturating_sub(2),
            &hunk_hdr,
            hdr_style,
        );
        cur_y += 1;

        for line in &hunk.lines {
            if cur_y >= inner.y + inner.height {
                break;
            }
            let (text, style) = match line {
                HunkLine::Context(s) => (format!("  {s}"), Style::fg(Color::White)),
                HunkLine::Delete(s) => (format!(" -{s}"), Style::fg(Color::Red)),
                HunkLine::Insert(s) => (format!(" +{s}"), Style::fg(Color::Green)),
            };
            buf.draw_text(inner.x + 1, cur_y, inner.width.saturating_sub(2), &text, style);
            cur_y += 1;
        }
    }
}
