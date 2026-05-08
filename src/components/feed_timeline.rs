//! S14 Feed Timeline screen. Renders the [`Timeline`] produced by
//! [`crate::feed_timeline::walk`] as a scrollable table with cursor
//! plus selection-detail line. Loading state is driven by
//! `:feed-timeline <owner> <topic>` (which spawns the walk and pushes
//! the result through an mpsc channel into `App`'s tick handler —
//! same shape as the durability_tx → S13 Watchlist plumbing).

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

use super::Component;
use crate::action::Action;
use crate::feed_timeline::{Timeline, TimelineEntry, format_age_secs};
use crate::theme;

/// Screen state. The walk happens in a background task; results
/// land here via `set_loading` → `set_timeline` / `set_error`.
pub struct FeedTimeline {
    /// `Some(timeline)` once the walk completes successfully.
    timeline: Option<Timeline>,
    /// `Some(message)` when the walk errored. Mutually exclusive
    /// with `timeline` — a fresh walk clears both.
    error: Option<String>,
    /// `true` while a walk is in flight. Drives the spinner glyph
    /// in the header.
    loading: bool,
    /// Header strip set when the verb kicks off a walk so the
    /// screen shows the operator-supplied owner/topic immediately,
    /// even before the latest-index probe completes.
    pending_label: Option<String>,
    /// Cursor row in the entries list.
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

    /// Called by `App` when `:feed-timeline <owner> <topic>` kicks
    /// off a walk. Clears prior state so the operator doesn't see
    /// stale entries while the new walk is in flight.
    pub fn set_loading(&mut self, label: impl Into<String>) {
        self.timeline = None;
        self.error = None;
        self.loading = true;
        self.pending_label = Some(label.into());
        self.selected = 0;
    }

    /// Walk completed cleanly — replace the displayed entries.
    pub fn set_timeline(&mut self, t: Timeline) {
        self.loading = false;
        self.error = None;
        self.timeline = Some(t);
        self.selected = 0;
    }

    /// Walk failed — surface the operator-facing reason.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.loading = false;
        self.timeline = None;
        self.error = Some(msg.into());
    }

    /// Reference under the cursor — used by future "c=copy" /
    /// "Enter=inspect" key bindings. Returns `None` when there is no
    /// timeline, no entries, or the selected row has no reference.
    pub fn selected_reference(&self) -> Option<&str> {
        self.timeline
            .as_ref()
            .and_then(|t| t.entries.get(self.selected))
            .and_then(|e| e.reference_hex.as_deref())
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
        } else if let Some(tm) = &self.timeline {
            header_line.push(Span::raw(format!(
                "  owner=0x{}  topic={}  latest=idx{}  · {} entries",
                short_hex(&tm.owner_hex, 12),
                short_hex(&tm.topic_hex, 8),
                tm.latest_index,
                tm.entries.len(),
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
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let mut body_lines: Vec<Line> = Vec::new();
        body_lines.push(Line::from(Span::styled(
            "  INDEX     AGE      SIZE   TYPE      REF / ERROR",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        )));
        match &self.timeline {
            Some(tm) if !tm.entries.is_empty() => {
                for (i, e) in tm.entries.iter().enumerate() {
                    body_lines.push(render_row(e, now, i == self.selected, t));
                }
            }
            Some(_) => {
                body_lines.push(Line::from(Span::styled(
                    "  (no entries — feed exists but every fetch errored)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                )));
            }
            None if self.loading => { /* spinner already in header */ }
            None => {}
        }
        frame.render_widget(Paragraph::new(body_lines), chunks[1]);

        // Selected-line detail (full reference when present).
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

        // Footer — keymap.
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

impl FeedTimeline {
    fn selected_entry(&self) -> Option<&TimelineEntry> {
        self.timeline
            .as_ref()
            .and_then(|t| t.entries.get(self.selected))
    }
}

fn render_row<'a>(e: &'a TimelineEntry, now: u64, is_selected: bool, t: &theme::Theme) -> Line<'a> {
    let age = e
        .timestamp_unix
        .map(|ts| format_age_secs(now.saturating_sub(ts)))
        .unwrap_or_else(|| "—".to_string());
    let kind = if e.error.is_some() {
        "miss"
    } else if e.reference_hex.is_some() {
        "ref"
    } else {
        "raw"
    };
    let body = match (&e.error, &e.reference_hex) {
        (Some(err), _) => format!("[{err}]"),
        (_, Some(r)) => short_hex(r, 12),
        (_, None) => format!("payload {}B", e.payload_bytes.saturating_sub(8)),
    };
    let row_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else if e.error.is_some() {
        Style::default().fg(t.dim)
    } else {
        Style::default()
    };
    Line::from(vec![Span::styled(
        format!(
            "  {:>6}  {:>10}  {:>4}  {:<8}  {body}",
            e.index, age, e.payload_bytes, kind,
        ),
        row_style,
    )])
}

fn short_hex(hex: &str, len: usize) -> String {
    let s = hex.trim_start_matches("0x");
    if s.len() > len {
        format!("{}…", &s[..len])
    } else {
        s.to_string()
    }
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
        // Row 1 has no reference (raw payload).
        s.selected = 1;
        assert!(s.selected_reference().is_none());
    }

    #[test]
    fn keypress_does_not_advance_past_last_row() {
        let mut s = FeedTimeline::new();
        s.set_timeline(timeline(vec![entry(1, None, None), entry(0, None, None)]));
        // Press Down twice — should clamp at 1.
        for _ in 0..5 {
            s.handle_key_event(KeyEvent::from(KeyCode::Down)).unwrap();
        }
        assert_eq!(s.selected, 1);
    }
}
