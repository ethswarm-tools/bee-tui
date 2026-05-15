//! S14 Feed Timeline screen. Per-row formatting + `view_for` live in
//! [`bee_cockpit_core::views::feed_timeline`]; this module owns the
//! screen-state machine (loading / error / pending-label / cursor)
//! and the ratatui draw path.

use std::any::Any;
use std::time::SystemTime;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

pub use bee_cockpit_core::views::feed_timeline::{
    FeedRowView, FeedTimelineHeader, FeedTimelineView, row_view, short_hex, view_for,
};

use super::Component;
use crate::action::Action;
use crate::feed_timeline::{Timeline, TimelineEntry};
use crate::theme;

pub struct FeedTimeline {
    timeline: Option<Timeline>,
    error: Option<String>,
    loading: bool,
    pending_label: Option<String>,
    selected: usize,
}

impl Default for FeedTimeline {
    fn default() -> Self {
        Self::new()
    }
}

impl FeedTimeline {
    pub fn new() -> Self {
        Self {
            timeline: None,
            error: None,
            loading: false,
            pending_label: None,
            selected: 0,
        }
    }

    pub fn set_loading(&mut self, label: impl Into<String>) {
        self.timeline = None;
        self.error = None;
        self.loading = true;
        self.pending_label = Some(label.into());
        self.selected = 0;
    }

    pub fn set_timeline(&mut self, t: Timeline) {
        self.loading = false;
        self.error = None;
        self.timeline = Some(t);
        self.selected = 0;
    }

    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.loading = false;
        self.timeline = None;
        self.error = Some(msg.into());
    }

    pub fn selected_reference(&self) -> Option<&str> {
        self.timeline
            .as_ref()
            .and_then(|t| t.entries.get(self.selected))
            .and_then(|e| e.reference_hex.as_deref())
    }

    fn selected_entry(&self) -> Option<&TimelineEntry> {
        self.timeline
            .as_ref()
            .and_then(|t| t.entries.get(self.selected))
    }
}

impl Component for FeedTimeline {
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        let len = self.timeline.as_ref().map(|t| t.entries.len()).unwrap_or(0);
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j') if len > 0 && self.selected + 1 < len => {
                self.selected += 1;
            }
            KeyCode::PageUp => {
                self.selected = self.selected.saturating_sub(10);
            }
            KeyCode::PageDown if len > 0 => {
                self.selected = (self.selected + 10).min(len.saturating_sub(1));
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let t = theme::active();
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let view = view_for(self.timeline.as_ref(), now);

        // Header
        let mut header_line = vec![Span::styled(
            "FEED TIMELINE",
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if self.loading {
            header_line.push(Span::raw("  "));
            header_line.push(Span::styled(
                format!("{} loading…", theme::spinner_glyph()),
                Style::default().fg(t.dim),
            ));
            if let Some(label) = &self.pending_label {
                header_line.push(Span::raw("  "));
                header_line.push(Span::styled(label.clone(), Style::default().fg(t.dim)));
            }
        } else if let Some(h) = &view.header {
            header_line.push(Span::raw(format!(
                "  owner=0x{}  topic={}  latest=idx{}  · {} entries",
                h.owner_hex_short, h.topic_hex_short, h.latest_index, h.entry_count,
            )));
        } else if let Some(e) = &self.error {
            header_line.push(Span::raw("  "));
            header_line.push(Span::styled(e.clone(), Style::default().fg(t.fail)));
        } else {
            header_line.push(Span::raw("  "));
            header_line.push(Span::styled(
                "type :feed-timeline <owner> <topic> [N] to load",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            ));
        }
        frame.render_widget(
            Paragraph::new(Line::from(header_line))
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Body — table of entries.
        let mut body_lines: Vec<Line> = Vec::new();
        body_lines.push(Line::from(Span::styled(
            "  INDEX     AGE      SIZE   TYPE      REF / ERROR",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        )));
        if view.header.is_some() && !view.rows.is_empty() {
            for (i, r) in view.rows.iter().enumerate() {
                body_lines.push(render_row(r, i == self.selected, t));
            }
        } else if view.header.is_some() {
            body_lines.push(Line::from(Span::styled(
                "  (no entries — feed exists but every fetch errored)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        frame.render_widget(Paragraph::new(body_lines), chunks[1]);

        let detail = match self.selected_entry() {
            Some(e) if e.reference_hex.is_some() => {
                format!("  selected: ref={}", e.reference_hex.as_deref().unwrap())
            }
            Some(e) => format!(
                "  selected: index={} · payload={}B · ts={}",
                e.index,
                e.payload_bytes,
                e.timestamp_unix
                    .map(|t| t.to_string())
                    .unwrap_or_else(|| "?".into()),
            ),
            None => String::new(),
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(detail, Style::default().fg(t.dim)))),
            chunks[2],
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ↑↓/jk ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" select  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" : ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" command  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit "),
            ])),
            chunks[3],
        );
        Ok(())
    }
}

fn render_row<'a>(r: &'a FeedRowView, is_selected: bool, t: &theme::Theme) -> Line<'a> {
    let row_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if r.is_error {
        Style::default().fg(t.dim)
    } else {
        Style::default()
    };
    Line::from(vec![Span::styled(
        format!(
            "  {:>6}  {:>10}  {:>4}  {:<8}  {}",
            r.index, r.age_label, r.size_label, r.kind, r.body,
        ),
        row_style,
    )])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::feed_timeline::{Timeline, TimelineEntry};

    fn entry(index: u64, ref_hex: Option<&str>, error: Option<&str>) -> TimelineEntry {
        TimelineEntry {
            index,
            timestamp_unix: Some(1_700_000_000),
            payload_bytes: 40,
            reference_hex: ref_hex.map(String::from),
            error: error.map(String::from),
        }
    }

    fn timeline(entries: Vec<TimelineEntry>) -> Timeline {
        Timeline {
            owner_hex: "1234567890abcdef1234567890abcdef12345678".into(),
            topic_hex: "a".repeat(64),
            latest_index: entries.first().map(|e| e.index).unwrap_or(0),
            index_next: entries.first().map(|e| e.index + 1).unwrap_or(0),
            entries,
            reached_requested: true,
        }
    }

    #[test]
    fn new_screen_has_no_timeline_no_error() {
        let s = FeedTimeline::new();
        assert!(s.timeline.is_none());
        assert!(s.error.is_none());
        assert!(!s.loading);
    }

    #[test]
    fn set_loading_clears_prior_state() {
        let mut s = FeedTimeline::new();
        s.set_timeline(timeline(vec![entry(0, None, None)]));
        s.set_loading("owner=abc topic=xyz");
        assert!(s.timeline.is_none());
        assert!(s.error.is_none());
        assert!(s.loading);
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn set_error_clears_loading() {
        let mut s = FeedTimeline::new();
        s.set_loading("owner=abc topic=xyz");
        s.set_error("/feeds/.../ failed: HTTP 500");
        assert!(!s.loading);
        assert!(s.error.is_some());
        assert!(s.timeline.is_none());
    }

    #[test]
    fn set_timeline_clears_loading() {
        let mut s = FeedTimeline::new();
        s.set_loading("owner=abc topic=xyz");
        s.set_timeline(timeline(vec![entry(0, Some(&"a".repeat(64)), None)]));
        assert!(!s.loading);
        assert!(s.timeline.is_some());
        assert!(s.error.is_none());
    }

    #[test]
    fn selected_reference_returns_cursor_ref() {
        let mut s = FeedTimeline::new();
        s.set_timeline(timeline(vec![
            entry(2, Some(&"a".repeat(64)), None),
            entry(1, None, None),
            entry(0, Some(&"b".repeat(64)), None),
        ]));
        assert_eq!(s.selected_reference(), Some("a".repeat(64).as_str()));
        s.selected = 2;
        assert_eq!(s.selected_reference(), Some("b".repeat(64).as_str()));
        s.selected = 1;
        assert!(s.selected_reference().is_none());
    }

    #[test]
    fn keypress_does_not_advance_past_last_row() {
        let mut s = FeedTimeline::new();
        s.set_timeline(timeline(vec![entry(1, None, None), entry(0, None, None)]));
        for _ in 0..5 {
            s.handle_key_event(KeyEvent::from(KeyCode::Down)).unwrap();
        }
        assert_eq!(s.selected, 1);
    }
}
