//! S9 — Tags / uploads screen (`docs/PLAN.md` § 8.S9).
//!
//! Renders a row per Bee tag (one tag = one upload). Tags expose
//! every counter Bee tracks during an upload's lifecycle:
//!
//! - **split** — chunks the file/collection got chunked into.
//! - **seen** — chunks Bee already had locally.
//! - **stored** — chunks now in local storage.
//! - **sent** — chunks pushed to the network.
//! - **synced** — chunks confirmed received by enough peers.
//!
//! The screen surfaces the per-stage progress so operators see
//! *which* phase a stuck upload is stuck in: still chunking? push
//! syncing? waiting for receipts?
//!
//! Render delegates to the pure [`Tags::view_for`] so the snapshot
//! tests in `tests/s9_tags_view.rs` pin every state transition
//! without launching a TUI.

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

use super::Component;
use crate::action::Action;
use crate::theme;
use crate::watch::TagsSnapshot;

use bee::api::Tag;

/// Per-tag lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TagStatus {
    /// `total <= 0` — Bee hasn't filled in the chunk count yet (the
    /// upload either hasn't started or used a streaming endpoint
    /// that doesn't pre-declare a total).
    Pending,
    /// Chunking in progress: `split < total`.
    Splitting,
    /// All chunks split; pushing them out: `sent < total`.
    Pushing,
    /// Pushed but waiting on enough receipts: `synced < total`.
    Syncing,
    /// `synced >= total > 0`. Done.
    Synced,
}

impl TagStatus {
    fn color(self) -> Color {
        match self {
            Self::Pending => theme::active().dim,
            Self::Splitting => theme::active().info,
            Self::Pushing => theme::active().warn,
            Self::Syncing => theme::active().warn,
            Self::Synced => theme::active().pass,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "· pending",
            Self::Splitting => "▒ splitting",
            Self::Pushing => "▒ pushing",
            Self::Syncing => "▒ syncing",
            Self::Synced => "✓ synced",
        }
    }
}

/// One row of the tags table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagRow {
    pub uid: u32,
    pub name: String,
    pub total: i64,
    pub split: i64,
    pub seen: i64,
    pub stored: i64,
    pub sent: i64,
    pub synced: i64,
    pub address_short: String,
    pub status: TagStatus,
    /// `synced / total` percentage in `0..=100`. `0` if total ≤ 0.
    pub completion_pct: u32,
    pub started_at: String,
}

/// Aggregate counters across the whole tag list — used by the
/// summary header.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TagsTotals {
    pub tags: usize,
    pub split: i64,
    pub sent: i64,
    pub synced: i64,
    /// Number of tags currently in `Splitting` / `Pushing` /
    /// `Syncing` — anything that's not Pending or Synced.
    pub active: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TagsView {
    pub rows: Vec<TagRow>,
    pub totals: TagsTotals,
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

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests.
    pub fn view_for(snap: &TagsSnapshot) -> TagsView {
        let mut rows: Vec<TagRow> = snap.tags.iter().map(row_from_tag).collect();
        // Newest tags first — Bee assigns monotonically increasing
        // UIDs, so sort by UID descending.
        rows.sort_by_key(|r| std::cmp::Reverse(r.uid));
        let totals = compute_totals(&rows);
        TagsView { rows, totals }
    }
}

fn row_from_tag(t: &Tag) -> TagRow {
    let status = classify_tag(t);
    let completion_pct = if t.total > 0 {
        let pct = (t.synced.max(0) as i128 * 100 / t.total as i128).min(100);
        pct as u32
    } else {
        0
    };
    let name = if t.name.is_empty() {
        format!("tag-{}", t.uid)
    } else {
        t.name.clone()
    };
    TagRow {
        uid: t.uid,
        name,
        total: t.total,
        split: t.split,
        seen: t.seen,
        stored: t.stored,
        sent: t.sent,
        synced: t.synced,
        address_short: short_ref(&t.address),
        status,
        completion_pct,
        started_at: t.started_at.clone(),
    }
}

pub fn classify_tag(t: &Tag) -> TagStatus {
    if t.total <= 0 {
        return TagStatus::Pending;
    }
    if t.synced >= t.total {
        return TagStatus::Synced;
    }
    if t.split < t.total {
        return TagStatus::Splitting;
    }
    if t.sent < t.total {
        return TagStatus::Pushing;
    }
    TagStatus::Syncing
}

fn compute_totals(rows: &[TagRow]) -> TagsTotals {
    let mut totals = TagsTotals {
        tags: rows.len(),
        ..TagsTotals::default()
    };
    for r in rows {
        totals.split += r.split;
        totals.sent += r.sent;
        totals.synced += r.synced;
        if matches!(
            r.status,
            TagStatus::Splitting | TagStatus::Pushing | TagStatus::Syncing
        ) {
            totals.active += 1;
        }
    }
    totals
}

fn short_ref(s: &str) -> String {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.is_empty() {
        return "—".into();
    }
    if trimmed.len() > 12 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
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

        let view = Self::view_for(&self.snapshot);
        let t = theme::active();

        // Header
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
            header_l2.push(Span::styled("loading…", Style::default().fg(t.dim)));
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Pinned column header + scrollable body. j/k/PageUp/PageDown
        // move scroll_offset; the renderer here just clamps it to
        // the current row count and area height.
        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(chunks[1]);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  UID    NAME                ADDR          TOTAL   SPLIT  SENT   SYNCED   %    STATUS",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::BOLD),
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
            let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len());
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
                        Style::default().fg(r.status.color()),
                    ),
                    Span::styled(
                        r.status.label(),
                        Style::default()
                            .fg(r.status.color())
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
            let body = table_chunks[1];
            let visible_rows = body.height as usize;
            // No selection cursor on S9 — clamp_scroll with
            // selected=scroll_offset is a no-op except for the
            // total-rows clamp, which is exactly what we want here.
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

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(
                    " jk/PgUp/PgDn ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" scroll  "),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn tag_with(uid: u32, total: i64, split: i64, sent: i64, synced: i64) -> Tag {
        Tag {
            uid,
            name: format!("tag-{uid}"),
            total,
            split,
            seen: split,
            stored: split,
            sent,
            synced,
            address: "ee".repeat(32),
            started_at: "2024-05-07T12:00:00Z".into(),
        }
    }

    #[test]
    fn classify_pending_when_total_zero() {
        assert_eq!(classify_tag(&tag_with(1, 0, 0, 0, 0)), TagStatus::Pending);
    }

    #[test]
    fn classify_splitting_when_split_below_total() {
        assert_eq!(
            classify_tag(&tag_with(1, 100, 50, 0, 0)),
            TagStatus::Splitting
        );
    }

    #[test]
    fn classify_pushing_when_sent_below_total() {
        assert_eq!(
            classify_tag(&tag_with(1, 100, 100, 60, 0)),
            TagStatus::Pushing
        );
    }

    #[test]
    fn classify_syncing_when_synced_below_total() {
        assert_eq!(
            classify_tag(&tag_with(1, 100, 100, 100, 50)),
            TagStatus::Syncing
        );
    }

    #[test]
    fn classify_synced_when_complete() {
        assert_eq!(
            classify_tag(&tag_with(1, 100, 100, 100, 100)),
            TagStatus::Synced
        );
        // Synced past total (Bee occasionally over-reports) is also Synced.
        assert_eq!(
            classify_tag(&tag_with(1, 100, 100, 100, 105)),
            TagStatus::Synced
        );
    }

    #[test]
    fn short_ref_handles_empty() {
        assert_eq!(short_ref(""), "—");
    }
}
