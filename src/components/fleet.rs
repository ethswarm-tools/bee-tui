//! S15 — Fleet view. Pure view-data half lives in
//! [`bee_cockpit_core::views::fleet`]; this module owns the watch
//! subscription, the cursor, the resync request channel, and the
//! ratatui draw path.

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::fleet::{
    FleetHeader, FleetRowView, FleetView, format_ttl, view_for,
};

use super::Component;
use crate::action::Action;
use crate::fleet::{FleetSnapshot, FleetStatus};
use crate::theme;

pub struct Fleet {
    rx: watch::Receiver<FleetSnapshot>,
    snapshot: FleetSnapshot,
    active_node_name: String,
    selected: usize,
    /// Scroll offset (in rendered lines) keeping the cursored node
    /// visible when the fleet has more nodes than the viewport fits.
    scroll_offset: usize,
    /// Operator-triggered "re-poll now" signal. Set by the poller's
    /// `spawn_poller` return value; pressing `r` sends `()`.
    resync_tx: tokio::sync::mpsc::UnboundedSender<()>,
}

impl Fleet {
    pub fn new(
        rx: watch::Receiver<FleetSnapshot>,
        active_node_name: String,
        resync_tx: tokio::sync::mpsc::UnboundedSender<()>,
    ) -> Self {
        let snapshot = rx.borrow().clone();
        Self {
            rx,
            snapshot,
            active_node_name,
            selected: 0,
            scroll_offset: 0,
            resync_tx,
        }
    }

    pub fn set_active_node(&mut self, name: String) {
        self.active_node_name = name;
    }

    pub fn selected_name(&self) -> Option<String> {
        self.snapshot
            .rows
            .get(self.selected)
            .map(|r| r.name.clone())
    }

    pub fn cursor_down(&mut self) {
        if !self.snapshot.rows.is_empty() {
            self.selected = (self.selected + 1) % self.snapshot.rows.len();
        }
    }

    pub fn cursor_up(&mut self) {
        if !self.snapshot.rows.is_empty() {
            let n = self.snapshot.rows.len();
            self.selected = (self.selected + n - 1) % n;
        }
    }

    pub fn request_resync(&self) {
        let _ = self.resync_tx.send(());
    }

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Fleet::view_for` call sites resolve.
    pub fn view_for(snap: &FleetSnapshot, active_name: &str, selected: usize) -> FleetView {
        view_for(snap, active_name, selected)
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        if self.selected >= self.snapshot.rows.len() && !self.snapshot.rows.is_empty() {
            self.selected = self.snapshot.rows.len() - 1;
        }
    }
}

impl Component for Fleet {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.cursor_down();
                Ok(None)
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.cursor_up();
                Ok(None)
            }
            KeyCode::Char('r') => {
                self.request_resync();
                Ok(None)
            }
            KeyCode::Enter => Ok(self.selected_name().map(Action::SwitchContext)),
            _ => Ok(None),
        }
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let view = view_for(&self.snapshot, &self.active_node_name, self.selected);
        let t = theme::active();

        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

        let header = build_header_line(&view.header, t);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        let mut row_starts: Vec<usize> = Vec::with_capacity(view.rows.len());
        let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() * 2 + 1);
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!(
                    "{:<14} {:<42} {:<8} {:>6} {:>10} {:>8}",
                    "NAME", "ENDPOINT", "STATUS", "PEERS", "WORST TTL", "PING"
                ),
                Style::default().fg(t.dim),
            ),
        ]));
        for (i, row) in view.rows.iter().enumerate() {
            row_starts.push(lines.len());
            let cursor = if i == view.selected { "▸ " } else { "  " };
            let mut marks = String::new();
            if row.active {
                marks.push('●');
            }
            if row.default {
                marks.push('★');
            }
            let name_col = if marks.is_empty() {
                row.name.clone()
            } else {
                format!("{} {}", row.name, marks)
            };
            let status_fg = match row.status {
                FleetStatus::Pass => t.pass,
                FleetStatus::Warn => t.warn,
                FleetStatus::Fail => t.fail,
                FleetStatus::Unknown => t.dim,
            };
            let row_style = if i == view.selected {
                Style::default()
                    .fg(t.tab_active_fg)
                    .bg(t.tab_active_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            let url_col = truncate(&row.url, 42);
            lines.push(Line::from(vec![
                Span::styled(cursor.to_string(), row_style),
                Span::styled(
                    format!("{:<14} {:<42} ", truncate(&name_col, 14), url_col,),
                    row_style,
                ),
                Span::styled(
                    format!("{:<8}", row.status_label),
                    if i == view.selected {
                        row_style
                    } else {
                        Style::default().fg(status_fg).add_modifier(Modifier::BOLD)
                    },
                ),
                Span::styled(
                    format!(
                        " {:>6} {:>10} {:>8}",
                        row.peers_label, row.ttl_label, row.ping_label
                    ),
                    row_style,
                ),
            ]));
            if let Some(why) = &row.why {
                lines.push(Line::from(vec![
                    Span::raw("      "),
                    Span::styled(
                        format!("└─ {why}"),
                        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
        if view.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no nodes configured — add [[nodes]] entries to config.toml)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }

        let body = chunks[1];
        let visible_rows = body.height as usize;
        let visual_cursor = row_starts.get(view.selected).copied().unwrap_or(0);
        self.scroll_offset = super::scroll::clamp_scroll(
            visual_cursor,
            self.scroll_offset,
            visible_rows,
            lines.len(),
        );
        frame.render_widget(
            Paragraph::new(lines.clone()).scroll((self.scroll_offset as u16, 0)),
            body,
        );
        super::scroll::render_scrollbar(frame, body, self.scroll_offset, visible_rows, lines.len());

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ↑/↓ select   Enter switch context   r re-poll cursored row   ● active  ★ default",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            ))),
            chunks[2],
        );

        Ok(())
    }
}

fn build_header_line(h: &FleetHeader, t: &theme::Theme) -> Line<'static> {
    let mut spans = vec![
        Span::styled("FLEET", Style::default().add_modifier(Modifier::BOLD)),
        Span::raw("  ·  "),
        Span::styled(
            format!("{} configured", h.total),
            Style::default().fg(t.dim),
        ),
    ];
    if h.pass > 0 {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("{} pass", h.pass),
            Style::default().fg(t.pass).add_modifier(Modifier::BOLD),
        ));
    }
    if h.warn > 0 {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("{} warn", h.warn),
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ));
    }
    if h.fail > 0 {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("{} fail", h.fail),
            Style::default().fg(t.fail).add_modifier(Modifier::BOLD),
        ));
    }
    if h.unknown > 0 {
        spans.push(Span::raw("  ·  "));
        spans.push(Span::styled(
            format!("{} unknown", h.unknown),
            Style::default().fg(t.dim),
        ));
    }
    Line::from(spans)
}

fn truncate(s: &str, width: usize) -> String {
    if s.chars().count() <= width {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_keeps_short_strings_intact() {
        assert_eq!(truncate("short", 10), "short");
    }

    #[test]
    fn truncate_adds_ellipsis_when_overflowing() {
        let s = "this-is-a-fairly-long-endpoint-name-1633";
        let out = truncate(s, 20);
        assert_eq!(out.chars().count(), 20);
        assert!(out.ends_with('…'));
    }
}
