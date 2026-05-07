//! Shared scroll helpers for components with selectable / overflowing
//! lists (S2 stamps, S6 peers, S9 tags). Components track a `usize`
//! offset alongside their `selected` index; this module bundles the
//! "keep selected visible" math + the right-edge scrollbar render.
//!
//! Each helper is pure where possible so unit tests can exercise the
//! offset arithmetic without spinning up a Frame.

use ratatui::{
    Frame,
    layout::Rect,
    style::Style,
    widgets::{Scrollbar, ScrollbarOrientation, ScrollbarState},
};

use crate::theme;

/// Recompute `scroll_offset` so `selected` is inside the
/// `visible_rows` window. Pure — no Frame access.
///
/// - Cursor moved above the window: snap the window to the cursor.
/// - Cursor moved below the window: scroll just enough to keep it
///   visible (one-line bumps, not page jumps — feels smoother in
///   practice).
/// - List shrunk: clamp so the offset never points past the end.
pub fn clamp_scroll(
    selected: usize,
    scroll_offset: usize,
    visible_rows: usize,
    total_rows: usize,
) -> usize {
    let visible_rows = visible_rows.max(1);
    let max_offset = total_rows.saturating_sub(visible_rows);
    let mut offset = scroll_offset;
    if selected < offset {
        offset = selected;
    } else if selected >= offset + visible_rows {
        offset = selected + 1 - visible_rows;
    }
    offset.min(max_offset)
}

/// Render the right-edge scrollbar over `area` if the list overflows.
/// Silently no-ops when every row already fits — that's the common
/// case (most operators have <8 batches / <30 peers / <5 tags).
pub fn render_scrollbar(
    frame: &mut Frame,
    area: Rect,
    scroll_offset: usize,
    visible_rows: usize,
    total_rows: usize,
) {
    if total_rows <= visible_rows {
        return;
    }
    let t = theme::active();
    let mut state = ScrollbarState::new(total_rows.saturating_sub(visible_rows))
        .position(scroll_offset)
        .viewport_content_length(visible_rows);
    let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
        .style(Style::default().fg(t.dim))
        .thumb_style(Style::default().fg(t.accent))
        .begin_symbol(None)
        .end_symbol(None);
    frame.render_stateful_widget(scrollbar, area, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_keeps_selection_in_window_when_already_visible() {
        // selected=3, offset=0, visible=10 — already in window.
        assert_eq!(clamp_scroll(3, 0, 10, 50), 0);
    }

    #[test]
    fn clamp_advances_window_when_selection_moves_below() {
        // selected=12, visible=10 → offset = 12 + 1 - 10 = 3
        assert_eq!(clamp_scroll(12, 0, 10, 50), 3);
    }

    #[test]
    fn clamp_snaps_window_up_when_selection_moves_above() {
        // selected=2, offset=10 — cursor went up; snap to it.
        assert_eq!(clamp_scroll(2, 10, 10, 50), 2);
    }

    #[test]
    fn clamp_holds_offset_within_max_when_list_shrinks() {
        // total=20, visible=10 → max_offset=10. Stale offset=15
        // must be clamped down even though the cursor isn't outside
        // (e.g. selected=14 was valid before the list shrank to 20).
        assert_eq!(clamp_scroll(14, 15, 10, 20), 10);
    }

    #[test]
    fn clamp_yields_zero_when_list_fits() {
        // visible=10, total=5 — never need to scroll.
        assert_eq!(clamp_scroll(4, 0, 10, 5), 0);
        assert_eq!(clamp_scroll(4, 7, 10, 5), 0); // even if offset is bogus
    }

    #[test]
    fn clamp_handles_zero_visible_rows_without_panic() {
        // Vertical layout can momentarily yield height=0 during
        // resize; treat as visible_rows=1 for safety.
        let _ = clamp_scroll(3, 0, 0, 50);
    }
}
