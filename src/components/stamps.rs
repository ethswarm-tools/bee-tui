//! S2 — Stamps screen (`docs/PLAN.md` § 8.S2). Pure view-data half
//! (StampStatus, StampRow, StampDrillView, StampEconomics,
//! WorstBucket, rows_for, compute_drill_view, format_bytes, the
//! TTL-threshold constants + `format_ttl_seconds`) lives in
//! [`bee_cockpit_core::views::stamps`] and [`bee_cockpit_core::stamps`].
//! This module owns the API client handle, the drill-fetch channels,
//! the cursor, and the ratatui draw path.

use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::{mpsc, watch};

pub use bee_cockpit_core::stamps::{TOPUP_SOON_SECS, TOPUP_URGENT_SECS, format_ttl_seconds};
pub use bee_cockpit_core::views::stamps::{
    FILL_BIN_LABELS, StampDrillView, StampEconomics, StampRow, StampStatus, WorstBucket,
    compute_drill_view, format_bytes, rows_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::StampsSnapshot;

use bee::postage::PostageBatchBuckets;
use bee::swarm::BatchId;

fn status_color(s: StampStatus) -> Color {
    let t = theme::active();
    match s {
        StampStatus::Pending => t.info,
        StampStatus::Expired => t.fail,
        StampStatus::Critical => t.fail,
        StampStatus::Skewed => t.warn,
        StampStatus::Healthy => t.pass,
    }
}

/// Drill-pane state machine. `Idle` keeps the regular table
/// rendered; the other variants replace it with the drill view.
#[derive(Debug, Clone)]
pub enum DrillState {
    Idle,
    Loading {
        batch_id: BatchId,
    },
    Loaded {
        batch_id: BatchId,
        view: StampDrillView,
    },
    Failed {
        batch_id: BatchId,
        error: String,
    },
}

type DrillFetchResult = (BatchId, std::result::Result<PostageBatchBuckets, String>);

pub struct Stamps {
    client: Arc<ApiClient>,
    rx: watch::Receiver<StampsSnapshot>,
    snapshot: StampsSnapshot,
    selected: usize,
    scroll_offset: usize,
    drill: DrillState,
    fetch_tx: mpsc::UnboundedSender<DrillFetchResult>,
    fetch_rx: mpsc::UnboundedReceiver<DrillFetchResult>,
}

impl Stamps {
    pub fn new(client: Arc<ApiClient>, rx: watch::Receiver<StampsSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        let (fetch_tx, fetch_rx) = mpsc::unbounded_channel();
        Self {
            client,
            rx,
            snapshot,
            selected: 0,
            scroll_offset: 0,
            drill: DrillState::Idle,
            fetch_tx,
            fetch_rx,
        }
    }

    /// Re-export of core's pure row builder as an inherent function
    /// so existing `Stamps::rows_for` call sites resolve.
    pub fn rows_for(snap: &StampsSnapshot) -> Vec<StampRow> {
        rows_for(snap)
    }

    /// Re-export of core's pure drill-view builder as an inherent
    /// function so existing `Stamps::compute_drill_view` call sites
    /// (notably the snapshot test file) resolve.
    pub fn compute_drill_view(
        buckets: &PostageBatchBuckets,
        batch: Option<&bee::postage::PostageBatch>,
    ) -> StampDrillView {
        compute_drill_view(buckets, batch)
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        let n = self.snapshot.batches.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    fn drain_fetches(&mut self) {
        while let Ok((batch_id, result)) = self.fetch_rx.try_recv() {
            match &self.drill {
                DrillState::Loading { batch_id: pending } if *pending == batch_id => {}
                _ => continue,
            }
            self.drill = match result {
                Ok(buckets) => {
                    let batch = self
                        .snapshot
                        .batches
                        .iter()
                        .find(|b| b.batch_id == batch_id);
                    DrillState::Loaded {
                        batch_id,
                        view: compute_drill_view(&buckets, batch),
                    }
                }
                Err(error) => DrillState::Failed { batch_id, error },
            };
        }
    }

    fn maybe_start_drill(&mut self) {
        if self.snapshot.batches.is_empty() {
            return;
        }
        let i = self.selected.min(self.snapshot.batches.len() - 1);
        let batch_id = self.snapshot.batches[i].batch_id;
        if let DrillState::Loading { batch_id: pending } = &self.drill {
            if *pending == batch_id {
                return;
            }
        }
        let client = self.client.clone();
        let tx = self.fetch_tx.clone();
        tokio::spawn(async move {
            let res = client
                .bee()
                .postage()
                .get_postage_batch_buckets(&batch_id)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send((batch_id, res));
        });
        self.drill = DrillState::Loading { batch_id };
    }
}

fn fill_bar(pct: u32, width: usize) -> String {
    let filled = ((pct as usize) * width) / 100;
    let mut bar = String::with_capacity(width);
    for _ in 0..filled.min(width) {
        bar.push('▇');
    }
    for _ in filled.min(width)..width {
        bar.push('░');
    }
    bar
}

impl Component for Stamps {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
            self.drain_fetches();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if matches!(
            self.drill,
            DrillState::Loaded { .. } | DrillState::Loading { .. } | DrillState::Failed { .. }
        ) && matches!(key.code, KeyCode::Esc)
        {
            self.drill = DrillState::Idle;
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                let n = self.snapshot.batches.len();
                if n > 0 && self.selected + 1 < n {
                    self.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.maybe_start_drill();
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let t = theme::active();
        let count = self.snapshot.batches.len();
        let mut header_l1 = vec![
            Span::styled("STAMPS", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {count} batch(es)")),
        ];
        if let DrillState::Loaded { batch_id, .. }
        | DrillState::Loading { batch_id }
        | DrillState::Failed { batch_id, .. } = &self.drill
        {
            let hex = batch_id.to_hex();
            header_l1.push(Span::raw("   · drill "));
            header_l1.push(Span::styled(hex, Style::default().fg(t.info)));
        }
        let header_l1 = Line::from(header_l1);
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

        match &self.drill {
            DrillState::Idle => self.draw_table(frame, chunks[1]),
            DrillState::Loading { .. } => {
                let msg = Line::from(Span::styled(
                    "  fetching /stamps/<id>/buckets…  (Esc cancel)",
                    Style::default().fg(t.dim),
                ));
                frame.render_widget(Paragraph::new(msg), chunks[1]);
            }
            DrillState::Failed { error, .. } => {
                let msg = Line::from(vec![
                    Span::raw("  drill failed: "),
                    Span::styled(error.clone(), Style::default().fg(t.fail)),
                    Span::raw("    (Esc to dismiss)"),
                ]);
                frame.render_widget(Paragraph::new(msg), chunks[1]);
            }
            DrillState::Loaded { view, .. } => self.draw_drill(frame, chunks[1], view),
        }

        if matches!(self.drill, DrillState::Idle) && !self.snapshot.batches.is_empty() {
            let i = self.selected.min(self.snapshot.batches.len() - 1);
            let b = &self.snapshot.batches[i];
            let label = if b.label.is_empty() {
                "(unlabeled)".to_string()
            } else {
                b.label.clone()
            };
            let detail = Line::from(vec![
                Span::styled("  selected: ", Style::default().fg(t.dim)),
                Span::styled(b.batch_id.to_hex(), Style::default().fg(t.info)),
                Span::raw("  "),
                Span::styled(label, Style::default().fg(t.dim)),
            ]);
            frame.render_widget(Paragraph::new(detail), chunks[2]);
        }

        let footer = match &self.drill {
            DrillState::Idle => Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(
                    " ↑↓/jk ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" select  "),
                Span::styled(" ↵ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" drill  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(" I/M ", Style::default().fg(t.dim)),
                Span::raw(" immutable / mutable "),
            ]),
            _ => Line::from(vec![
                Span::styled(" Esc ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" close drill  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit "),
            ]),
        };
        frame.render_widget(Paragraph::new(footer), chunks[3]);

        Ok(())
    }
}

impl Stamps {
    fn draw_table(&mut self, frame: &mut Frame, area: Rect) {
        let t = theme::active();
        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "   LABEL                BATCH        VOLUME      WORST BUCKET                TTL         STATUS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ))),
            table_chunks[0],
        );

        if self.snapshot.batches.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "   (no batches yet — buy one with swarm-cli or `bee stamps buy`)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ))),
                table_chunks[1],
            );
            return;
        }

        let mut lines: Vec<Line> = Vec::new();
        let mut row_starts: Vec<usize> = Vec::new();
        for (i, r) in rows_for(&self.snapshot).into_iter().enumerate() {
            row_starts.push(lines.len());
            let bar = fill_bar(r.worst_bucket_pct, 8);
            let immut_glyph = if r.immutable { "I" } else { "M" };
            let cursor = if i == self.selected {
                format!("{} ", t.glyphs.cursor)
            } else {
                "  ".to_string()
            };
            lines.push(Line::from(vec![
                Span::styled(
                    cursor,
                    Style::default()
                        .fg(if i == self.selected { t.accent } else { t.dim })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{:<20}", truncate(&r.label, 20)),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:<13}", r.batch_id_short)),
                Span::raw(format!("{:<12}", r.volume)),
                Span::styled(
                    format!("{bar} {:>3}% ({})", r.worst_bucket_pct, r.worst_bucket_raw),
                    Style::default().fg(bucket_color(r.worst_bucket_pct)),
                ),
                Span::raw("    "),
                Span::raw(format!("{:<10} ", r.ttl)),
                Span::styled(immut_glyph, Style::default().fg(t.dim)),
                Span::raw(" "),
                Span::styled(
                    r.status.label(),
                    Style::default()
                        .fg(status_color(r.status))
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(why) = r.why {
                lines.push(Line::from(vec![
                    Span::raw(format!("        {} ", t.glyphs.continuation)),
                    Span::styled(
                        why,
                        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }

        let visual_cursor = row_starts.get(self.selected).copied().unwrap_or(0);
        let body = table_chunks[1];
        let visible_rows = body.height as usize;
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
    }

    fn draw_drill(&self, frame: &mut Frame, area: Rect, view: &StampDrillView) {
        let t = theme::active();
        let mut lines: Vec<Line> = Vec::new();
        let total_buckets: u32 = view.fill_distribution.iter().sum();
        lines.push(Line::from(vec![
            Span::raw("  depth "),
            Span::styled(
                format!("{}", view.depth),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   bucket-depth "),
            Span::styled(
                format!("{}", view.bucket_depth),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   per-bucket cap "),
            Span::styled(
                format!("{}", view.upper_bound),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(
                format!("{} buckets", total_buckets),
                Style::default().fg(t.dim),
            ),
        ]));
        lines.push(Line::from(vec![
            Span::raw("  total chunks "),
            Span::styled(
                format!("{}", view.total_chunks),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw(" / "),
            Span::styled(
                format!("{}", view.theoretical_capacity),
                Style::default().fg(t.dim),
            ),
            Span::raw("   worst bucket "),
            Span::styled(
                format!("{}%", view.worst_pct),
                Style::default()
                    .fg(bucket_color(view.worst_pct))
                    .add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(e) = &view.economics {
            lines.push(Line::from(vec![
                Span::raw("  paid "),
                Span::styled(
                    e.bzz_paid.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("   volume "),
                Span::styled(
                    e.volume_humanised.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw("   "),
                Span::styled(e.bzz_per_gib.clone(), Style::default().fg(t.dim)),
            ]));
        }
        lines.push(Line::from(""));

        lines.push(Line::from(Span::styled(
            "  FILL %       COUNT   DISTRIBUTION",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        )));
        let max_bin = view
            .fill_distribution
            .iter()
            .copied()
            .max()
            .unwrap_or(1)
            .max(1);
        for (idx, count) in view.fill_distribution.iter().enumerate() {
            let label = FILL_BIN_LABELS[idx];
            let bar_width = ((u64::from(*count) * 30) / u64::from(max_bin)) as usize;
            let bar: String = std::iter::repeat_n('▇', bar_width).collect();
            let bin_color = match idx {
                5 => t.fail,
                4 => t.warn,
                _ => t.pass,
            };
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::raw(format!("{label:<10}  ")),
                Span::styled(
                    format!("{count:>5}   "),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(bar, Style::default().fg(bin_color)),
            ]));
        }
        lines.push(Line::from(""));

        if !view.worst_buckets.is_empty() {
            lines.push(Line::from(Span::styled(
                "  WORST BUCKETS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )));
            for w in &view.worst_buckets {
                if w.collisions == 0 {
                    break;
                }
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("#{:<8}", w.bucket_id)),
                    Span::raw(format!("{:>4} / {}    ", w.collisions, view.upper_bound)),
                    Span::styled(
                        format!("{}%", w.pct),
                        Style::default()
                            .fg(bucket_color(w.pct))
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }
        }

        frame.render_widget(Paragraph::new(lines), area);
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

fn bucket_color(pct: u32) -> Color {
    let t = theme::active();
    if pct >= 95 {
        t.fail
    } else if pct >= 80 {
        t.warn
    } else {
        t.pass
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fill_bar_clamps_to_width() {
        assert_eq!(fill_bar(0, 8), "░░░░░░░░");
        assert_eq!(fill_bar(50, 8), "▇▇▇▇░░░░");
        assert_eq!(fill_bar(100, 8), "▇▇▇▇▇▇▇▇");
        assert_eq!(fill_bar(150, 8), "▇▇▇▇▇▇▇▇");
    }
}
