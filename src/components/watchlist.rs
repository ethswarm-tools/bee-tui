//! S13 — Durability Watchlist screen. Pure view-data half lives in
//! [`bee_cockpit_core::views::watchlist`]; this module owns the
//! ring buffer of `DurabilityResult`s, the cursor, and the ratatui
//! draw / key path.

use std::collections::VecDeque;
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

pub use bee_cockpit_core::views::watchlist::{MAX_ROWS, WatchlistRow, WatchlistView, view_for};

use super::Component;
use crate::action::Action;
use crate::durability::DurabilityResult;
use crate::theme;

pub struct Watchlist {
    rows: VecDeque<DurabilityResult>,
    selected: usize,
}

impl Default for Watchlist {
    fn default() -> Self {
        Self::new()
    }
}

impl Watchlist {
    pub fn new() -> Self {
        Self {
            rows: VecDeque::with_capacity(MAX_ROWS),
            selected: 0,
        }
    }

    /// Push a fresh durability-check result onto the front of the
    /// list. Bounded — when the ring is full the oldest entry is
    /// evicted. The cursor stays anchored on whatever the operator
    /// was looking at, unless the eviction happened to push it off
    /// the end.
    pub fn record(&mut self, result: DurabilityResult) {
        if self.rows.len() == MAX_ROWS {
            self.rows.pop_back();
        }
        self.rows.push_front(result);
        if self.selected >= self.rows.len() && !self.rows.is_empty() {
            self.selected = self.rows.len() - 1;
        }
    }

    /// Re-export of core's pure view computation, kept as an
    /// inherent function so existing `Watchlist::view_for` call
    /// sites keep working without changing imports.
    pub fn view_for(rows: &VecDeque<DurabilityResult>, now: SystemTime) -> WatchlistView {
        view_for(rows, now)
    }

    fn cached_view(&self) -> WatchlistView {
        view_for(&self.rows, SystemTime::now())
    }
}

impl Component for Watchlist {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Down | KeyCode::Char('j')
                if !self.rows.is_empty() && self.selected + 1 < self.rows.len() =>
            {
                self.selected += 1;
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let t = theme::active();
        let view = self.cached_view();
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let header = if view.rows.is_empty() {
            Line::from(Span::styled(
                "no durability checks yet — type :durability-check <ref> to record one",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            ))
        } else {
            Line::from(vec![
                Span::styled(format!(" {} ", view.rows.len()), Style::default().fg(t.dim)),
                Span::raw("checks · "),
                Span::styled(
                    format!("{} ", view.healthy_count),
                    Style::default().fg(t.pass).add_modifier(Modifier::BOLD),
                ),
                Span::raw("healthy · "),
                Span::styled(
                    format!("{} ", view.unhealthy_count),
                    if view.unhealthy_count == 0 {
                        Style::default().fg(t.dim)
                    } else {
                        Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                    },
                ),
                Span::raw("unhealthy"),
            ])
        };
        frame.render_widget(
            Paragraph::new(header).block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() + 1);
        if !view.rows.is_empty() {
            if self.selected >= view.rows.len() {
                self.selected = view.rows.len() - 1;
            }
            for (i, row) in view.rows.iter().enumerate() {
                let cursor_marker = if i == self.selected { "▸ " } else { "  " };
                let status_style = if row.healthy {
                    Style::default().fg(t.pass).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                };
                let kind = if row.root_is_manifest {
                    "manifest"
                } else {
                    "chunk   "
                };
                lines.push(Line::from(vec![
                    Span::styled(cursor_marker.to_string(), Style::default().fg(t.accent)),
                    Span::styled(format!("{:<10}", row.status_label), status_style),
                    Span::raw("  "),
                    Span::styled(kind.to_string(), Style::default().fg(t.dim)),
                    Span::raw("  "),
                    Span::raw(short_hex(&row.reference_hex, 8)),
                    Span::raw("  "),
                    Span::styled(row.detail.clone(), Style::default().fg(t.dim)),
                    Span::raw("  "),
                    Span::styled(
                        format!("{}s ago", row.age_seconds),
                        Style::default().fg(t.dim),
                    ),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), chunks[1]);

        if !view.rows.is_empty() {
            let row = &view.rows[self.selected.min(view.rows.len() - 1)];
            frame.render_widget(
                Paragraph::new(Line::from(vec![
                    Span::styled("  selected: ", Style::default().fg(t.dim)),
                    Span::styled(row.reference_hex.clone(), Style::default().fg(t.info)),
                ])),
                chunks[2],
            );
        }

        let footer = Line::from(vec![
            Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" switch screen  "),
            Span::styled(
                " ↑↓/jk ",
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::raw(" select  "),
            Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" help  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" quit  "),
            Span::styled(
                ":durability-check <ref> to record",
                Style::default().fg(t.dim),
            ),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[3]);
        Ok(())
    }
}

fn short_hex(s: &str, n: usize) -> String {
    if s.len() <= n * 2 + 1 {
        s.to_string()
    } else {
        format!("{}…{}", &s[..n], &s[s.len() - n..])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bee::swarm::Reference;
    use std::time::Duration;

    fn make_result(healthy: bool, secs_ago: u64) -> DurabilityResult {
        DurabilityResult {
            reference: Reference::from_hex(&"a".repeat(64)).unwrap(),
            started_at: SystemTime::now() - Duration::from_secs(secs_ago),
            duration_ms: 200,
            chunks_total: 4,
            chunks_lost: if healthy { 0 } else { 1 },
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        }
    }

    #[test]
    fn record_evicts_oldest_when_full() {
        let mut wl = Watchlist::new();
        for i in 0..MAX_ROWS + 5 {
            let r = make_result(true, i as u64);
            wl.record(r);
        }
        assert_eq!(wl.rows.len(), MAX_ROWS);
    }

    #[test]
    fn record_pushes_newest_to_front() {
        let mut wl = Watchlist::new();
        wl.record(make_result(true, 100));
        wl.record(make_result(false, 50));
        let v = wl.cached_view();
        assert!(v.rows[0].status_label.contains("UNHEALTHY"));
        assert!(v.rows[1].status_label.contains("OK"));
    }

    #[test]
    fn short_hex_truncates_long_strings() {
        let long = "a".repeat(64);
        let s = short_hex(&long, 8);
        assert!(s.contains('…'));
    }
}
