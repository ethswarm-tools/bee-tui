//! S11 — Pins screen (`docs/PLAN.md` § 8.S11; Tier 3.A).
//!
//! Promotes the `:pins-check` command's read-only output into a
//! sortable table view. The list of pinned references comes from
//! `/pins` (polled on the slow tier; pin sets are
//! operator-driven). Per-pin integrity comes from `/pins/check`,
//! which walks the entire chunk graph for each pin — too expensive
//! to auto-poll. The cockpit triggers it on demand when the
//! operator presses `Enter` (single pin) or `c` (all pins).
//!
//! ## Render path
//!
//! Pure [`Pins::view_for`] turns `(snapshot, check_state, sort_mode)`
//! into a [`PinsView`]. The renderer just walks the view's `rows`
//! and the snapshot tests in `tests/s11_pins_view.rs` pin sort
//! ordering / count summaries / unhealthy-flag rendering without
//! launching a TUI.
//!
//! ## What's intentionally out of scope (v1)
//!
//! - **No pin/unpin actions.** Adding a pin is a write op gated by
//!   the same "no funds-bearing actions" rule as cashout / topup;
//!   if you want to pin something, do it from `swarm-cli`.
//! - **No download retry.** Unhealthy pins are surfaced; fixing
//!   them (re-uploading missing chunks) is operator territory.

use std::collections::HashMap;
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

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::PinsSnapshot;

use bee::api::PinIntegrity;
use bee::swarm::Reference;

/// Per-pin integrity-check status. `Idle` = operator hasn't asked
/// for a check yet; `Checking` = a `/pins/check` call is in flight.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckState {
    Idle,
    Checking,
    Ok {
        total: u64,
        missing: u64,
        invalid: u64,
    },
    Failed(String),
}

impl CheckState {
    /// `true` when we have a result and any chunks are missing or
    /// invalid. `Idle`/`Checking`/`Failed` return `false` — they
    /// don't claim health one way or the other.
    pub fn is_unhealthy(&self) -> bool {
        matches!(self, Self::Ok { missing, invalid, .. } if *missing > 0 || *invalid > 0)
    }
    /// `true` when the pin checked clean.
    pub fn is_healthy(&self) -> bool {
        matches!(
            self,
            Self::Ok {
                missing: 0,
                invalid: 0,
                ..
            }
        )
    }
}

/// One row of the pins table. `reference_short` is the standard
/// `prefix…suffix` form for table display; the renderer can fall
/// back to the full hex for click-drag copy in a footer line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinRow {
    pub reference: Reference,
    pub reference_short: String,
    pub check: CheckState,
}

/// How the rows are ordered. Operator cycles via `s`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortMode {
    /// Bee's response order (the default — gives operators the same
    /// order they'd see from `curl /pins`).
    Reference,
    /// Unhealthy → unchecked → healthy. Surfaces the rows that
    /// matter for an operator who suspects local chunk loss.
    BadFirst,
    /// Largest pins first by `total` chunk count. Useful for
    /// understanding which pin set dominates local reserve usage.
    TotalChunks,
}

impl SortMode {
    fn next(self) -> Self {
        match self {
            Self::Reference => Self::BadFirst,
            Self::BadFirst => Self::TotalChunks,
            Self::TotalChunks => Self::Reference,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Reference => "ref order",
            Self::BadFirst => "bad first",
            Self::TotalChunks => "by size",
        }
    }
}

/// View fed to the renderer. Pure transform of
/// `(snapshot, check_state, sort_mode)`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PinsView {
    pub rows: Vec<PinRow>,
    pub sort: SortMode,
    pub total_pins: usize,
    pub healthy: usize,
    pub unhealthy: usize,
    pub unchecked: usize,
}

type FetchResult = (Reference, std::result::Result<PinIntegrity, String>);

pub struct Pins {
    client: Arc<ApiClient>,
    rx: watch::Receiver<PinsSnapshot>,
    snapshot: PinsSnapshot,
    /// Per-reference check state. Survives snapshot refreshes — we
    /// don't want a fresh `/pins` poll to wipe the integrity result
    /// the operator just spent 30s computing.
    checks: HashMap<Reference, CheckState>,
    selected: usize,
    scroll_offset: usize,
    sort: SortMode,
    fetch_tx: mpsc::UnboundedSender<FetchResult>,
    fetch_rx: mpsc::UnboundedReceiver<FetchResult>,
}

impl Pins {
    pub fn new(client: Arc<ApiClient>, rx: watch::Receiver<PinsSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        let (fetch_tx, fetch_rx) = mpsc::unbounded_channel();
        Self {
            client,
            rx,
            snapshot,
            checks: HashMap::new(),
            selected: 0,
            scroll_offset: 0,
            sort: SortMode::Reference,
            fetch_tx,
            fetch_rx,
        }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        // Clamp the cursor if the pin set shrank.
        let n = self.snapshot.pins.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Drain integrity-check fetches that completed since the last
    /// tick. Late results are still applied — pins are stable
    /// across snapshot refreshes so a stale-looking result is still
    /// useful (vs. the S2 drill, which races a different batch).
    fn drain_fetches(&mut self) {
        while let Ok((reference, result)) = self.fetch_rx.try_recv() {
            let next = match result {
                Ok(p) => CheckState::Ok {
                    total: p.total,
                    missing: p.missing,
                    invalid: p.invalid,
                },
                Err(e) => CheckState::Failed(e),
            };
            self.checks.insert(reference, next);
        }
    }

    /// Pure view builder for snapshot tests + the renderer.
    pub fn view_for(
        snap: &PinsSnapshot,
        checks: &HashMap<Reference, CheckState>,
        sort: SortMode,
    ) -> PinsView {
        let mut rows: Vec<PinRow> = snap
            .pins
            .iter()
            .map(|r| {
                let check = checks.get(r).cloned().unwrap_or(CheckState::Idle);
                PinRow {
                    reference: r.clone(),
                    reference_short: short_ref(&r.to_hex()),
                    check,
                }
            })
            .collect();

        match sort {
            SortMode::Reference => {} // already in Bee's response order
            SortMode::BadFirst => {
                rows.sort_by_key(|r| match &r.check {
                    CheckState::Ok {
                        missing, invalid, ..
                    } if *missing > 0 || *invalid > 0 => 0,
                    CheckState::Failed(_) => 1,
                    CheckState::Idle => 2,
                    CheckState::Checking => 3,
                    CheckState::Ok { .. } => 4,
                });
            }
            SortMode::TotalChunks => {
                rows.sort_by_key(|r| match &r.check {
                    CheckState::Ok { total, .. } => std::cmp::Reverse(*total),
                    _ => std::cmp::Reverse(0),
                });
            }
        }

        let mut healthy = 0;
        let mut unhealthy = 0;
        let mut unchecked = 0;
        for r in &rows {
            if r.check.is_healthy() {
                healthy += 1;
            } else if r.check.is_unhealthy() {
                unhealthy += 1;
            } else if matches!(r.check, CheckState::Idle) {
                unchecked += 1;
            }
        }

        PinsView {
            total_pins: rows.len(),
            rows,
            sort,
            healthy,
            unhealthy,
            unchecked,
        }
    }

    /// Spawn a `/pins/check?ref=X` request for the row under the
    /// cursor. No-op if a check is already in flight for that ref.
    fn check_selected(&mut self) {
        if self.snapshot.pins.is_empty() {
            return;
        }
        let i = self.selected.min(self.snapshot.pins.len() - 1);
        let reference = self.snapshot.pins[i].clone();
        if matches!(self.checks.get(&reference), Some(CheckState::Checking)) {
            return;
        }
        self.checks.insert(reference.clone(), CheckState::Checking);
        let client = self.client.clone();
        let tx = self.fetch_tx.clone();
        let task_ref = reference.clone();
        tokio::spawn(async move {
            let r = client
                .bee()
                .api()
                .check_pins(Some(&task_ref))
                .await
                .map_err(|e| e.to_string())
                .and_then(|mut entries| {
                    entries
                        .pop()
                        .ok_or_else(|| "Bee returned no integrity entry".to_string())
                });
            let _ = tx.send((task_ref, r));
        });
    }

    /// `c` — kick off a check for every currently-Idle pin. Bounded
    /// concurrency: we spawn one task per pin, but Bee serialises
    /// them server-side anyway. Already-Checking / Ok / Failed pins
    /// are skipped.
    fn check_all(&mut self) {
        let pending: Vec<Reference> = self
            .snapshot
            .pins
            .iter()
            .filter(|r| matches!(self.checks.get(*r), None | Some(CheckState::Idle)))
            .cloned()
            .collect();
        for reference in pending {
            self.checks.insert(reference.clone(), CheckState::Checking);
            let client = self.client.clone();
            let tx = self.fetch_tx.clone();
            let task_ref = reference;
            tokio::spawn(async move {
                let r = client
                    .bee()
                    .api()
                    .check_pins(Some(&task_ref))
                    .await
                    .map_err(|e| e.to_string())
                    .and_then(|mut entries| {
                        entries
                            .pop()
                            .ok_or_else(|| "Bee returned no integrity entry".to_string())
                    });
                let _ = tx.send((task_ref, r));
            });
        }
    }
}

/// `prefix…suffix` form used in the table display. Long enough to
/// disambiguate, short enough to leave room for the integrity columns.
fn short_ref(hex: &str) -> String {
    let trimmed = hex.trim_start_matches("0x");
    if trimmed.len() > 14 {
        format!("{}…{}", &trimmed[..8], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
    }
}

impl Component for Pins {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
            self.drain_fetches();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                let n = self.snapshot.pins.len();
                if n > 0 && self.selected + 1 < n {
                    self.selected += 1;
                }
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
            }
            KeyCode::Enter => {
                self.check_selected();
            }
            KeyCode::Char('c') => {
                self.check_all();
            }
            KeyCode::Char('s') => {
                self.sort = self.sort.next();
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(0),    // body
            Constraint::Length(1), // selected detail (full reference)
            Constraint::Length(1), // footer
        ])
        .split(area);

        let view = Self::view_for(&self.snapshot, &self.checks, self.sort);
        let t = theme::active();

        // Header
        let header_l1 = Line::from(vec![
            Span::styled("PINS", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {} pinned", view.total_pins)),
            Span::raw("   "),
            Span::styled(format!("✓ {}", view.healthy), Style::default().fg(t.pass)),
            Span::raw("   "),
            Span::styled(format!("✗ {}", view.unhealthy), Style::default().fg(t.fail)),
            Span::raw("   "),
            Span::styled(format!("? {}", view.unchecked), Style::default().fg(t.dim)),
            Span::raw("   sort "),
            Span::styled(view.sort.label(), Style::default().fg(t.info)),
        ]);
        let header_l2 = match &self.snapshot.last_error {
            Some(err) => {
                let (color, msg) = theme::classify_header_error(err);
                Line::from(Span::styled(msg, Style::default().fg(color)))
            }
            None if !self.snapshot.is_loaded() => Line::from(Span::styled(
                format!("{} loading…", theme::spinner_glyph()),
                Style::default().fg(t.dim),
            )),
            None => Line::from(Span::styled(
                "  Press Enter to integrity-check the highlighted pin, c for all, s to re-sort.",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )),
        };
        frame.render_widget(
            Paragraph::new(vec![header_l1, header_l2])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Body — pinned column header + scrollable rows.
        let body = chunks[1];
        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(body);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "   REFERENCE         TOTAL    MISSING    INVALID    STATUS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ))),
            table_chunks[0],
        );

        if view.rows.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "   (no pinned references — pin one with `swarm-cli pin add`)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ))),
                table_chunks[1],
            );
        } else {
            let rows_area = table_chunks[1];
            let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() + 2);
            for (i, r) in view.rows.iter().enumerate() {
                let cursor = if i == self.selected {
                    format!("{} ", t.glyphs.cursor)
                } else {
                    "  ".to_string()
                };
                let (total, missing, invalid, status_text, status_color) = match &r.check {
                    CheckState::Idle => (
                        "—".to_string(),
                        "—".to_string(),
                        "—".to_string(),
                        "? unchecked".to_string(),
                        t.dim,
                    ),
                    CheckState::Checking => (
                        "—".to_string(),
                        "—".to_string(),
                        "—".to_string(),
                        format!("{} checking…", theme::spinner_glyph()),
                        t.info,
                    ),
                    CheckState::Ok {
                        total,
                        missing,
                        invalid,
                    } => {
                        let healthy = *missing == 0 && *invalid == 0;
                        (
                            total.to_string(),
                            missing.to_string(),
                            invalid.to_string(),
                            if healthy {
                                "✓ healthy".into()
                            } else {
                                "✗ degraded".into()
                            },
                            if healthy { t.pass } else { t.fail },
                        )
                    }
                    CheckState::Failed(err) => (
                        "—".to_string(),
                        "—".to_string(),
                        "—".to_string(),
                        format!("✗ check failed: {err}"),
                        t.fail,
                    ),
                };
                lines.push(Line::from(vec![
                    Span::styled(
                        cursor,
                        Style::default()
                            .fg(if i == self.selected { t.accent } else { t.dim })
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(
                        format!("{:<18}", r.reference_short),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{total:>6}     ")),
                    Span::raw(format!("{missing:>6}     ")),
                    Span::raw(format!("{invalid:>6}     ")),
                    Span::styled(
                        status_text,
                        Style::default()
                            .fg(status_color)
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
            }

            // Visual-line scroll based on row selection — same pattern
            // as S6 / S2.
            let visible_rows = rows_area.height as usize;
            self.scroll_offset = super::scroll::clamp_scroll(
                self.selected,
                self.scroll_offset,
                visible_rows,
                lines.len(),
            );
            frame.render_widget(
                Paragraph::new(lines.clone()).scroll((self.scroll_offset as u16, 0)),
                rows_area,
            );
            super::scroll::render_scrollbar(
                frame,
                rows_area,
                self.scroll_offset,
                visible_rows,
                lines.len(),
            );
        }

        // Selected detail — full reference of the highlighted row,
        // click-drag selectable so operators can copy without
        // shrinking the column or sacrificing the table layout.
        if !view.rows.is_empty() {
            let i = self.selected.min(view.rows.len() - 1);
            let row = &view.rows[i];
            let detail = Line::from(vec![
                Span::styled("  selected: ", Style::default().fg(t.dim)),
                Span::styled(row.reference.to_hex(), Style::default().fg(t.info)),
            ]);
            frame.render_widget(Paragraph::new(detail), chunks[2]);
        }

        // Footer
        let footer = Line::from(vec![
            Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" switch screen  "),
            Span::styled(
                " ↑↓/jk ",
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::raw(" select  "),
            Span::styled(" ↵ ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" check pin  "),
            Span::styled(" c ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" check all  "),
            Span::styled(" s ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" sort  "),
            Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" help  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" quit "),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[3]);

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn r(byte: u8) -> Reference {
        Reference::new(&[byte; 32]).unwrap()
    }

    fn ok(total: u64, missing: u64, invalid: u64) -> CheckState {
        CheckState::Ok {
            total,
            missing,
            invalid,
        }
    }

    #[test]
    fn check_state_health_predicates() {
        assert!(ok(10, 0, 0).is_healthy());
        assert!(!ok(10, 0, 0).is_unhealthy());
        assert!(ok(10, 1, 0).is_unhealthy());
        assert!(ok(10, 0, 1).is_unhealthy());
        assert!(!CheckState::Idle.is_healthy());
        assert!(!CheckState::Idle.is_unhealthy());
        assert!(!CheckState::Checking.is_unhealthy());
    }

    #[test]
    fn view_for_empty_snapshot_renders_zero_counts() {
        let snap = PinsSnapshot::default();
        let view = Pins::view_for(&snap, &HashMap::new(), SortMode::Reference);
        assert_eq!(view.total_pins, 0);
        assert_eq!(view.healthy, 0);
        assert_eq!(view.unhealthy, 0);
        assert_eq!(view.unchecked, 0);
    }

    #[test]
    fn view_for_counts_health_buckets() {
        let snap = PinsSnapshot {
            pins: vec![r(1), r(2), r(3), r(4)],
            ..PinsSnapshot::default()
        };
        let mut checks = HashMap::new();
        checks.insert(r(1), ok(100, 0, 0)); // healthy
        checks.insert(r(2), ok(100, 5, 0)); // unhealthy
        checks.insert(r(3), CheckState::Failed("nope".into())); // not unchecked, not healthy
        // r(4) → unchecked (no entry)
        let view = Pins::view_for(&snap, &checks, SortMode::Reference);
        assert_eq!(view.total_pins, 4);
        assert_eq!(view.healthy, 1);
        assert_eq!(view.unhealthy, 1);
        assert_eq!(view.unchecked, 1);
    }

    #[test]
    fn view_for_default_sort_preserves_response_order() {
        let snap = PinsSnapshot {
            pins: vec![r(3), r(1), r(2)],
            ..PinsSnapshot::default()
        };
        let view = Pins::view_for(&snap, &HashMap::new(), SortMode::Reference);
        assert_eq!(view.rows[0].reference, r(3));
        assert_eq!(view.rows[1].reference, r(1));
        assert_eq!(view.rows[2].reference, r(2));
    }

    #[test]
    fn view_for_bad_first_surfaces_unhealthy_then_failed_then_unchecked_then_healthy() {
        let snap = PinsSnapshot {
            pins: vec![r(1), r(2), r(3), r(4), r(5)],
            ..PinsSnapshot::default()
        };
        let mut checks = HashMap::new();
        checks.insert(r(1), ok(10, 0, 0)); // healthy
        checks.insert(r(2), ok(10, 1, 0)); // unhealthy → first
        checks.insert(r(3), CheckState::Failed("e".into())); // failed → second
        checks.insert(r(4), CheckState::Checking); // checking → fourth
        // r(5) unchecked → third
        let view = Pins::view_for(&snap, &checks, SortMode::BadFirst);
        let order: Vec<_> = view.rows.iter().map(|r| r.reference.clone()).collect();
        assert_eq!(order, vec![r(2), r(3), r(5), r(4), r(1)]);
    }

    #[test]
    fn view_for_total_chunks_sorts_descending_with_unchecked_last() {
        let snap = PinsSnapshot {
            pins: vec![r(1), r(2), r(3), r(4)],
            ..PinsSnapshot::default()
        };
        let mut checks = HashMap::new();
        checks.insert(r(1), ok(50, 0, 0));
        checks.insert(r(2), ok(500, 0, 0));
        checks.insert(r(3), ok(5, 0, 0));
        // r(4) → unchecked (counts as 0 → goes last)
        let view = Pins::view_for(&snap, &checks, SortMode::TotalChunks);
        let order: Vec<_> = view.rows.iter().map(|r| r.reference.clone()).collect();
        // r(2)=500, r(1)=50, r(3)=5, r(4)=unchecked
        assert_eq!(order, vec![r(2), r(1), r(3), r(4)]);
    }

    #[test]
    fn sort_mode_cycles() {
        assert_eq!(SortMode::Reference.next(), SortMode::BadFirst);
        assert_eq!(SortMode::BadFirst.next(), SortMode::TotalChunks);
        assert_eq!(SortMode::TotalChunks.next(), SortMode::Reference);
    }

    #[test]
    fn short_ref_keeps_short_strings_intact() {
        assert_eq!(short_ref("abcd"), "abcd");
        assert_eq!(short_ref("0x1234"), "1234");
        assert_eq!(
            short_ref("aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"),
            "aabbccdd…8899"
        );
    }
}
