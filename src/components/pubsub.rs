//! S15 Pubsub watch screen. Per-row formatting + filter logic live
//! in [`bee_cockpit_core::views::pubsub`]; this module owns the
//! ring buffer, the cursor, the active-subscription count, and the
//! ratatui draw path.

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

pub use bee_cockpit_core::views::pubsub::{
    PubsubRowView, PubsubView, format_clock, match_filter, row_view, short_hex, view_for,
};

use super::Component;
use crate::action::Action;
use crate::pubsub::{MAX_MESSAGES, PubsubKind, PubsubMessage};
use crate::theme;

pub struct Pubsub {
    /// Newest at the front; capped at [`MAX_MESSAGES`].
    rows: VecDeque<PubsubMessage>,
    selected: usize,
    /// Scroll offset (in rendered lines) keeping the cursored message
    /// visible when the timeline overflows the body pane.
    scroll_offset: usize,
    active_subs: usize,
    /// Optional case-insensitive substring filter, pre-lowercased.
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
            scroll_offset: 0,
            active_subs: 0,
            filter: None,
        }
    }

    pub fn set_filter(&mut self, substring: Option<String>) {
        self.filter = substring.map(|s| s.to_ascii_lowercase());
        self.selected = 0;
        self.scroll_offset = 0;
    }

    /// True iff `msg` matches the active filter (or no filter is
    /// set). Thin wrapper over core's `match_filter` so existing
    /// call sites resolve.
    pub fn matches_filter(&self, msg: &PubsubMessage) -> bool {
        match_filter(msg, self.filter.as_deref())
    }

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
        let mut selected_line = 0usize;
        if self.rows.is_empty() {
            body.push(Line::from(Span::styled(
                "  (no messages yet — start a subscription with :pubsub-pss <topic> or :pubsub-gsoc <owner> <id>)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
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
                    if *i == self.selected {
                        selected_line = body.len();
                    }
                    body.push(render_row(&row_view(msg), *i == self.selected, t));
                }
            }
        }
        let body_area = chunks[1];
        let visible_rows = body_area.height as usize;
        self.scroll_offset = super::scroll::clamp_scroll(
            selected_line,
            self.scroll_offset,
            visible_rows,
            body.len(),
        );
        frame.render_widget(
            Paragraph::new(body.clone()).scroll((self.scroll_offset as u16, 0)),
            body_area,
        );
        super::scroll::render_scrollbar(
            frame,
            body_area,
            self.scroll_offset,
            visible_rows,
            body.len(),
        );

        let detail = match self.rows.get(self.selected) {
            Some(msg) => {
                let rv = row_view(msg);
                vec![
                    Line::from(Span::styled(
                        format!("  channel: {} · {} bytes", rv.channel, rv.payload_bytes),
                        Style::default().fg(t.dim),
                    )),
                    Line::from(Span::styled(
                        format!("  data: {}", rv.preview_long),
                        Style::default().fg(t.dim),
                    )),
                ]
            }
            None => vec![Line::from(""), Line::from("")],
        };
        frame.render_widget(Paragraph::new(detail), chunks[2]);

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

fn render_row(rv: &PubsubRowView, is_selected: bool, t: &theme::Theme) -> Line<'static> {
    let row_style = if is_selected {
        Style::default().add_modifier(Modifier::REVERSED)
    } else {
        match rv.kind {
            PubsubKind::Pss => Style::default(),
            PubsubKind::Gsoc => Style::default().fg(t.info),
        }
    };
    Line::from(vec![Span::styled(
        format!(
            "  {}   {}  {:<12}  {:>4}   {}",
            rv.time_label, rv.kind_label, rv.channel_short, rv.payload_bytes, rv.preview_short,
        ),
        row_style,
    )])
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
}
