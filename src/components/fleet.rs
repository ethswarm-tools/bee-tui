//! S15 — Fleet view.
//!
//! Renders the [`crate::fleet::FleetSnapshot`] produced by the
//! fleet poller. One row per `[[nodes]]` entry, with aggregate
//! status / peers / worst-batch TTL / ping. Operators land here to
//! answer "is anything red?" across every node they run without
//! switching contexts.
//!
//! Pure render: snapshot → [`FleetView`] (in this file) → ratatui
//! widgets. `tests/s15_fleet_view.rs` asserts the view shape across
//! representative fleet states without spinning up a TUI.

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

use super::Component;
use crate::action::Action;
use crate::fleet::{FleetRow, FleetSnapshot, FleetStatus};
use crate::theme;

/// Pure, render-ready view of the fleet.
#[derive(Debug, Clone, PartialEq)]
pub struct FleetView {
    pub header: FleetHeader,
    pub rows: Vec<FleetRowView>,
    pub selected: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FleetHeader {
    pub total: usize,
    pub pass: usize,
    pub warn: usize,
    pub fail: usize,
    pub unknown: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FleetRowView {
    pub name: String,
    pub url: String,
    pub default: bool,
    pub active: bool,
    pub status: FleetStatus,
    pub status_label: String,
    pub peers_label: String,
    pub ttl_label: String,
    pub ping_label: String,
    pub why: Option<String>,
}

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

    /// Update which node is currently active (called by App when
    /// `:context` switches so the `●` marker follows).
    pub fn set_active_node(&mut self, name: String) {
        self.active_node_name = name;
    }

    /// Name of the row the cursor is on, used by App to drive
    /// Enter-to-switch.
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

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        if self.selected >= self.snapshot.rows.len() && !self.snapshot.rows.is_empty() {
            self.selected = self.snapshot.rows.len() - 1;
        }
    }

    pub fn view_for(snap: &FleetSnapshot, active_name: &str, selected: usize) -> FleetView {
        let (pass, warn, fail, unknown) = snap.counts();
        let rows = snap
            .rows
            .iter()
            .map(|r| row_view(r, active_name))
            .collect::<Vec<_>>();
        FleetView {
            header: FleetHeader {
                total: snap.rows.len(),
                pass,
                warn,
                fail,
                unknown,
            },
            rows,
            selected,
        }
    }
}

fn row_view(r: &FleetRow, active_name: &str) -> FleetRowView {
    let status_label = match r.status {
        FleetStatus::Pass => "pass".into(),
        FleetStatus::Warn => "warn".into(),
        FleetStatus::Fail => "fail".into(),
        FleetStatus::Unknown => "…loading".into(),
    };
    let peers_label = r.peers.map(|p| p.to_string()).unwrap_or_else(|| "—".into());
    let ttl_label = r
        .worst_ttl_secs
        .map(format_ttl)
        .unwrap_or_else(|| "—".into());
    let ping_label = r
        .ping_ms
        .map(|p| format!("{p}ms"))
        .unwrap_or_else(|| "—".into());
    FleetRowView {
        name: r.name.clone(),
        url: r.url.clone(),
        default: r.default,
        active: r.name == active_name,
        status: r.status,
        status_label,
        peers_label,
        ttl_label,
        ping_label,
        why: r.why.clone(),
    }
}

/// Human-readable TTL — same convention as the Stamps screen, just
/// compacter (one unit max).
fn format_ttl(secs: u64) -> String {
    if secs >= 86_400 {
        format!("{}d", secs / 86_400)
    } else if secs >= 3_600 {
        format!("{}h", secs / 3_600)
    } else if secs >= 60 {
        format!("{}m", secs / 60)
    } else {
        format!("{secs}s")
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
        let view = Self::view_for(&self.snapshot, &self.active_node_name, self.selected);
        let t = theme::active();

        let chunks = Layout::vertical([
            Constraint::Length(2), // header line + spacer
            Constraint::Min(0),    // rows
            Constraint::Length(1), // key hint (pinned)
        ])
        .split(area);

        let header = build_header_line(&view.header, t);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // `row_starts[i]` is the line index where node `i`'s main row
        // begins — lets `clamp_scroll` keep the cursored node visible
        // even though non-pass rows render a second continuation line.
        let mut row_starts: Vec<usize> = Vec::with_capacity(view.rows.len());
        let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() * 2 + 1);
        // Column header
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
            // Truncate the URL to fit the column, since some operators
            // run nodes behind long DNS names + ports.
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
        // Keep the cursored node's main row on screen. Falls back to
        // the top when the fleet is empty.
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
    use crate::fleet::{FleetRow, FleetSnapshot, FleetStatus};
    use std::time::Instant;

    fn row(name: &str, status: FleetStatus, peers: Option<u64>, ttl: Option<u64>) -> FleetRow {
        FleetRow {
            name: name.into(),
            url: format!("http://{name}.example:1633"),
            default: false,
            status,
            peers,
            worst_ttl_secs: ttl,
            ping_ms: Some(12),
            warming_up: false,
            last_probe: Some(Instant::now()),
            why: match status {
                FleetStatus::Fail => Some("0 peers — isolated".into()),
                FleetStatus::Warn => Some("only 2 peers (< 4)".into()),
                _ => None,
            },
        }
    }

    #[test]
    fn view_header_counts_partition() {
        let snap = FleetSnapshot {
            rows: vec![
                row("a", FleetStatus::Pass, Some(87), Some(86_400 * 30)),
                row("b", FleetStatus::Warn, Some(2), Some(86_400 * 30)),
                row("c", FleetStatus::Fail, Some(0), Some(86_400 * 30)),
            ],
            last_update: Some(Instant::now()),
        };
        let view = Fleet::view_for(&snap, "a", 0);
        assert_eq!(view.header.total, 3);
        assert_eq!(view.header.pass, 1);
        assert_eq!(view.header.warn, 1);
        assert_eq!(view.header.fail, 1);
        assert_eq!(view.header.unknown, 0);
    }

    #[test]
    fn view_active_row_is_marked() {
        let snap = FleetSnapshot {
            rows: vec![row("a", FleetStatus::Pass, Some(87), Some(86_400 * 30))],
            last_update: Some(Instant::now()),
        };
        let view = Fleet::view_for(&snap, "a", 0);
        assert!(view.rows[0].active);
    }

    #[test]
    fn view_inactive_row_is_not_marked() {
        let snap = FleetSnapshot {
            rows: vec![row("a", FleetStatus::Pass, Some(87), Some(86_400 * 30))],
            last_update: Some(Instant::now()),
        };
        let view = Fleet::view_for(&snap, "different-context", 0);
        assert!(!view.rows[0].active);
    }

    #[test]
    fn view_ttl_formatting_picks_largest_unit() {
        let snap = FleetSnapshot {
            rows: vec![
                row("days", FleetStatus::Pass, Some(87), Some(86_400 * 30)),
                row("hours", FleetStatus::Warn, Some(87), Some(3_600 * 14)),
                row("mins", FleetStatus::Fail, Some(0), Some(60 * 14)),
            ],
            last_update: Some(Instant::now()),
        };
        let view = Fleet::view_for(&snap, "", 0);
        assert_eq!(view.rows[0].ttl_label, "30d");
        assert_eq!(view.rows[1].ttl_label, "14h");
        assert_eq!(view.rows[2].ttl_label, "14m");
    }

    #[test]
    fn view_empty_peers_show_dash() {
        let snap = FleetSnapshot {
            rows: vec![row("down", FleetStatus::Fail, None, None)],
            last_update: Some(Instant::now()),
        };
        let view = Fleet::view_for(&snap, "", 0);
        assert_eq!(view.rows[0].peers_label, "—");
        assert_eq!(view.rows[0].ttl_label, "—");
    }

    #[test]
    fn format_ttl_handles_all_buckets() {
        assert_eq!(format_ttl(86_400 * 5), "5d");
        assert_eq!(format_ttl(3_600 * 3), "3h");
        assert_eq!(format_ttl(60 * 45), "45m");
        assert_eq!(format_ttl(42), "42s");
    }

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
