//! S9 — Tags / uploads screen (`docs/PLAN.md` § 8.S9). Pure view-data
//! half lives in [`bee_cockpit_core::views::tags`]; this module
//! draws the table and owns the watch-channel subscription + scroll
//! cursor.

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::tags::{
    TagRow, TagStatus, TagsTotals, TagsView, classify_tag, short_ref, view_for,
};

use super::Component;
use crate::action::Action;
use crate::theme;
use crate::watch::TagsSnapshot;

fn status_color(s: TagStatus) -> Color {
    match s {
        TagStatus::Pending => theme::active().dim,
        TagStatus::Splitting => theme::active().info,
        TagStatus::Pushing => theme::active().warn,
        TagStatus::Syncing => theme::active().warn,
        TagStatus::Synced => theme::active().pass,
    }
}

fn status_label(s: TagStatus) -> &'static str {
    match s {
        TagStatus::Pending => "· pending",
        TagStatus::Splitting => "▒ splitting",
        TagStatus::Pushing => "▒ pushing",
        TagStatus::Syncing => "▒ syncing",
        TagStatus::Synced => "✓ synced",
    }
}

pub struct Tags {
    rx: watch::Receiver<TagsSnapshot>,
    snapshot: TagsSnapshot,
    /// Visual-line scroll offset. No selection cursor on S9 yet —
    /// j/k / ↑↓ / PageUp/PageDown move this directly.
    scroll_offset: usize,
}

impl Tags {
    pub fn new(rx: watch::Receiver<TagsSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self {
            rx,
            snapshot,
            scroll_offset: 0,
        }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
    }

    /// Re-export of core's pure view computation, kept as an inherent
    /// function so existing `Tags::view_for` call sites keep working.
    pub fn view_for(snap: &TagsSnapshot) -> TagsView {
        view_for(snap)
    }
}

impl Component for Tags {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<Option<Action>> {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                self.scroll_offset = self.scroll_offset.saturating_add(1);
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
            KeyCode::PageDown => {
                self.scroll_offset = self.scroll_offset.saturating_add(10);
            }
            KeyCode::PageUp => {
                self.scroll_offset = self.scroll_offset.saturating_sub(10);
            }
            KeyCode::Home => {
                self.scroll_offset = 0;
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(0),    // table
            Constraint::Length(1), // footer
        ])
        .split(area);

        let view = view_for(&self.snapshot);
        let t = theme::active();

        let header_l1 = Line::from(vec![
            Span::styled(
                "TAGS / UPLOADS",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(format!("   {} tag(s)", view.totals.tags)),
            Span::styled(
                format!("   active {}", view.totals.active),
                Style::default().fg(t.warn),
            ),
            Span::styled(
                format!(
                    "   split {} · sent {} · synced {}",
                    view.totals.split, view.totals.sent, view.totals.synced
                ),
                Style::default().fg(t.dim),
            ),
        ]);
        let mut header_l2 = Vec::new();
        if let Some(err) = &self.snapshot.last_error {
            let (color, msg) = theme::classify_header_error(err);
            header_l2.push(Span::styled(msg, Style::default().fg(color)));
        } else if !self.snapshot.is_loaded() {
            header_l2.push(Span::styled(
                format!("{} loading…", theme::spinner_glyph()),
                Style::default().fg(t.dim),
            ));
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(chunks[1]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  UID    NAME                ADDR          TOTAL   SPLIT  SENT   SYNCED   %    STATUS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ))),
            table_chunks[0],
        );

        if view.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "  (no tags reported — uploads via swarm-cli or the API will appear here)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ))),
                table_chunks[1],
            );
        } else {
            let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() * 2);
            for r in &view.rows {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:<6} ", r.uid)),
                    Span::styled(
                        format!("{:<19} ", truncate(&r.name, 19)),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{:<13} ", r.address_short)),
                    Span::raw(format!("{:>5}  ", r.total)),
                    Span::raw(format!("{:>5}  ", r.split)),
                    Span::raw(format!("{:>5}  ", r.sent)),
                    Span::raw(format!("{:>6}   ", r.synced)),
                    Span::styled(
                        format!("{:>3}% ", r.completion_pct),
                        Style::default().fg(status_color(r.status)),
                    ),
                    Span::styled(
                        status_label(r.status),
                        Style::default()
                            .fg(status_color(r.status))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                if !r.address_full.is_empty() {
                    lines.push(Line::from(vec![
                        Span::styled("        ref 0x", Style::default().fg(t.dim)),
                        Span::styled(r.address_full.clone(), Style::default().fg(t.info)),
                    ]));
                }
            }
            let body = table_chunks[1];
            let visible_rows = body.height as usize;
            self.scroll_offset = super::scroll::clamp_scroll(
                self.scroll_offset,
                self.scroll_offset,
                visible_rows,
                lines.len(),
            );
            frame.render_widget(
                Paragraph::new(lines.clone()).scroll((self.scroll_offset as u16, 0)),
                body,
            );
            super::scroll::render_scrollbar(
                frame,
                body,
                self.scroll_offset,
                visible_rows,
                lines.len(),
            );
        }

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(
                    " jk/PgUp/PgDn ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" scroll  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled("stages: split → sent → synced", Style::default().fg(t.dim)),
            ])),
            chunks[2],
        );

        Ok(())
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}
