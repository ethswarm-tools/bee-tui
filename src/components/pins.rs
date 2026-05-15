//! S11 — Pins screen (`docs/PLAN.md` § 8.S11; Tier 3.A). Pure view-
//! data half lives in [`bee_cockpit_core::views::pins`]; this module
//! owns the API client handle, the watch subscription, the per-pin
//! integrity-check map, the sort cycler, and the ratatui draw path.

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

pub use bee_cockpit_core::views::pins::{
    CheckState, PinRow, PinsView, SortMode, short_ref, view_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::PinsSnapshot;

use bee::api::PinIntegrity;
use bee::swarm::Reference;

type FetchResult = (Reference, std::result::Result<PinIntegrity, String>);

pub struct Pins {
    client: Arc<ApiClient>,
    rx: watch::Receiver<PinsSnapshot>,
    snapshot: PinsSnapshot,
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

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Pins::view_for` call sites resolve.
    pub fn view_for(
        snap: &PinsSnapshot,
        checks: &HashMap<Reference, CheckState>,
        sort: SortMode,
    ) -> PinsView {
        view_for(snap, checks, sort)
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        let n = self.snapshot.pins.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

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
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let view = view_for(&self.snapshot, &self.checks, self.sort);
        let t = theme::active();

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

        if !view.rows.is_empty() {
            let i = self.selected.min(view.rows.len() - 1);
            let row = &view.rows[i];
            let detail = Line::from(vec![
                Span::styled("  selected: ", Style::default().fg(t.dim)),
                Span::styled(row.reference.to_hex(), Style::default().fg(t.info)),
            ]);
            frame.render_widget(Paragraph::new(detail), chunks[2]);
        }

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
