//! S15 Pubsub watch screen. Renders the merged timeline of PSS +
//! GSOC subscriptions managed by [`crate::pubsub`]. Newest message
//! at the top; the cursor lets the operator inspect a specific
//! row's full hex/ASCII payload via the detail line.

use std::any::Any;
use std::collections::VecDeque;

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
use crate::pubsub::{MAX_MESSAGES, PubsubKind, PubsubMessage, smart_preview};
use crate::theme;

pub struct Pubsub {
    /// Newest at the front; capped at [`MAX_MESSAGES`].
    rows: VecDeque<PubsubMessage>,
    selected: usize,
    /// Number of currently active subscriptions, displayed in the
    /// header. Updated by `App` via [`Self::set_active_count`] when
    /// subscriptions start / stop.
    active_subs: usize,
    /// Optional case-insensitive substring filter. When `Some`,
    /// only rows whose channel hex or smart-preview contains the
    /// substring are rendered; the underlying ring still receives
    /// every message (filtering is presentation-only).
    filter: Option<String>,
}

impl Default for Pubsub {
    fn default() -> Self {
        Self::new()
    }
}

impl Pubsub {
    pub fn new() -> Self {
        Self {
            rows: VecDeque::with_capacity(MAX_MESSAGES),
            selected: 0,
            active_subs: 0,
            filter: None,
        }
    }

    /// Set or clear the substring filter. `None` clears it.
    pub fn set_filter(&mut self, substring: Option<String>) {
        self.filter = substring.map(|s| s.to_ascii_lowercase());
        self.selected = 0;
    }

    /// True iff `msg` matches the active filter (or no filter is
    /// set). Pure for testability.
    pub fn matches_filter(&self, msg: &PubsubMessage) -> bool {
        let Some(needle) = self.filter.as_deref() else {
            return true;
        };
        if msg.channel.to_ascii_lowercase().contains(needle) {
            return true;
        }
        let preview = smart_preview(&msg.payload, 200).to_ascii_lowercase();
        preview.contains(needle)
    }

    /// Push a freshly-received message onto the front of the
    /// timeline. Bounded — when the ring is full the oldest entry
    /// is evicted. Cursor stays anchored on the row the operator
    /// was looking at unless the eviction pushed it off the end.
    pub fn record(&mut self, msg: PubsubMessage) {
        if self.rows.len() == MAX_MESSAGES {
            self.rows.pop_back();
        }
        self.rows.push_front(msg);
        if self.selected >= self.rows.len() && !self.rows.is_empty() {
            self.selected = self.rows.len() - 1;
        }
    }

    pub fn set_active_count(&mut self, n: usize) {
        self.active_subs = n;
    }
}

impl Component for Pubsub {
    fn as_any_mut(&mut self) -> Option<&mut dyn Any> {
        Some(self)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        let len = self.rows.len();
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
            KeyCode::Char('c') => {
                self.rows.clear();
                self.selected = 0;
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
            Constraint::Length(2),
            Constraint::Length(1),
        ])
        .split(area);

        // Header
        let mut header_spans = vec![
            Span::styled(
                "PUBSUB WATCH",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!(
                "  · {} active subs · {} messages",
                self.active_subs,
                self.rows.len(),
            )),
        ];
        if let Some(f) = &self.filter {
            header_spans.push(Span::styled(
                format!("  · filter: {f:?}"),
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            ));
        }
        let header_line = Line::from(header_spans);
        frame.render_widget(
            Paragraph::new(header_line).block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Body — most-recent-first message timeline.
        let mut body: Vec<Line> = Vec::with_capacity(self.rows.len() + 1);
        body.push(Line::from(Span::styled(
            "  TIME      KIND   CHANNEL      SIZE   PREVIEW",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        )));
        if self.rows.is_empty() {
            body.push(Line::from(Span::styled(
                "  (no messages yet — start a subscription with :pubsub-pss <topic> or :pubsub-gsoc <owner> <id>)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            // Pre-collect filtered rows so the cursor index lines up
            // with what's actually displayed.
            let visible: Vec<(usize, &PubsubMessage)> = self
                .rows
                .iter()
                .enumerate()
                .filter(|(_, m)| self.matches_filter(m))
                .collect();
            if visible.is_empty() {
                body.push(Line::from(Span::styled(
                    "  (filter matches no messages — :pubsub-filter-clear to clear)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                )));
            } else {
                for (i, msg) in &visible {
                    body.push(render_row(msg, *i == self.selected, t));
                }
            }
        }
        frame.render_widget(Paragraph::new(body), chunks[1]);

        // Detail — full preview of the cursored row's payload + channel.
        let detail = match self.rows.get(self.selected) {
            Some(msg) => {
                let preview_long = smart_preview(&msg.payload, 200);
                vec![
                    Line::from(Span::styled(
                        format!("  channel: {} · {} bytes", msg.channel, msg.payload.len(),),
                        Style::default().fg(t.dim),
                    )),
                    Line::from(Span::styled(
                        format!("  data: {preview_long}"),
                        Style::default().fg(t.dim),
                    )),
                ]
            }
            None => vec![Line::from(""), Line::from("")],
        };
        frame.render_widget(Paragraph::new(detail), chunks[2]);

        // Footer — keymap.
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(
                    " ↑↓/jk ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" select  "),
                Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" clear timeline  "),
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

fn render_row(msg: &PubsubMessage, is_selected: bool, t: &theme::Theme) -> Line<'static> {
    let time_str = format_clock(msg.received_at);
    let kind_str = match msg.kind {
        PubsubKind::Pss => "PSS ",
        PubsubKind::Gsoc => "GSOC",
    };
    let chan_short = short_hex(&msg.channel, 12);
    let preview = smart_preview(&msg.payload, 50);
    let row_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        match msg.kind {
            PubsubKind::Pss => Style::default(),
            PubsubKind::Gsoc => Style::default().fg(t.info),
        }
    };
    Line::from(vec![Span::styled(
        format!(
            "  {time_str}   {kind_str}  {chan_short:<12}  {:>4}   {preview}",
            msg.payload.len(),
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

fn format_clock(t: std::time::SystemTime) -> String {
    use std::time::{Duration, UNIX_EPOCH};
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    let h = (secs / 3600) % 24;
    let m = (secs / 60) % 60;
    let s = secs % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::SystemTime;

    fn msg(kind: PubsubKind, channel: &str, payload: &[u8]) -> PubsubMessage {
        PubsubMessage {
            received_at: SystemTime::now(),
            kind,
            channel: channel.to_string(),
            payload: payload.to_vec(),
        }
    }

    #[test]
    fn record_pushes_newest_to_front() {
        let mut s = Pubsub::new();
        s.record(msg(PubsubKind::Pss, "topic-1", b"first"));
        s.record(msg(PubsubKind::Pss, "topic-1", b"second"));
        assert_eq!(s.rows[0].payload, b"second");
        assert_eq!(s.rows[1].payload, b"first");
    }

    #[test]
    fn record_evicts_oldest_when_full() {
        let mut s = Pubsub::new();
        for i in 0..(MAX_MESSAGES + 5) {
            s.record(msg(PubsubKind::Pss, "topic", format!("msg-{i}").as_bytes()));
        }
        assert_eq!(s.rows.len(), MAX_MESSAGES);
        // Newest at the front: msg-(MAX_MESSAGES + 4).
        let head = std::str::from_utf8(&s.rows[0].payload).unwrap();
        assert_eq!(head, format!("msg-{}", MAX_MESSAGES + 4));
    }

    #[test]
    fn clear_key_empties_timeline() {
        let mut s = Pubsub::new();
        s.record(msg(PubsubKind::Pss, "topic", b"data"));
        assert_eq!(s.rows.len(), 1);
        s.handle_key_event(KeyEvent::from(KeyCode::Char('c')))
            .unwrap();
        assert!(s.rows.is_empty());
        assert_eq!(s.selected, 0);
    }

    #[test]
    fn cursor_clamps_at_last_row() {
        let mut s = Pubsub::new();
        s.record(msg(PubsubKind::Pss, "topic", b"a"));
        s.record(msg(PubsubKind::Pss, "topic", b"b"));
        for _ in 0..10 {
            s.handle_key_event(KeyEvent::from(KeyCode::Down)).unwrap();
        }
        assert_eq!(s.selected, 1);
    }

    #[test]
    fn set_active_count_updates_header_state() {
        let mut s = Pubsub::new();
        s.set_active_count(3);
        assert_eq!(s.active_subs, 3);
    }

    #[test]
    fn matches_filter_no_filter_set_passes_everything() {
        let s = Pubsub::new();
        let m = msg(PubsubKind::Pss, "abc123", b"hello");
        assert!(s.matches_filter(&m));
    }

    #[test]
    fn matches_filter_substring_in_channel() {
        let mut s = Pubsub::new();
        s.set_filter(Some("CAFE".to_string()));
        let m = msg(PubsubKind::Pss, "cafebabe1234", b"unrelated");
        assert!(s.matches_filter(&m), "channel match (case-insensitive)");
    }

    #[test]
    fn matches_filter_substring_in_preview() {
        let mut s = Pubsub::new();
        s.set_filter(Some("ping".to_string()));
        let m = msg(PubsubKind::Pss, "topic", b"{\"event\":\"ping\"}");
        assert!(s.matches_filter(&m), "preview match");
    }

    #[test]
    fn matches_filter_no_match_drops() {
        let mut s = Pubsub::new();
        s.set_filter(Some("xyz".to_string()));
        let m = msg(PubsubKind::Pss, "topic", b"hello");
        assert!(!s.matches_filter(&m));
    }
}
