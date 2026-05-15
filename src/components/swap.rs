//! S3 — SWAP / cheques screen (`docs/PLAN.md` § 8.S3). Pure view-data
//! half lives in [`bee_cockpit_core::views::swap`]; this module owns
//! the watch subscriptions, the two-pane focus toggle, the scroll
//! offsets, and the ratatui draw path.

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::swap::{
    CheckRow, ChequebookCard, MarketTile, SettlementRow, SwapStatus, SwapView, format_plur,
    view_for, view_for_no_market,
};

use super::Component;
use crate::action::Action;
use crate::theme;
use crate::watch::SwapSnapshot;

fn status_color(s: SwapStatus) -> Color {
    match s {
        SwapStatus::Empty => theme::active().warn,
        SwapStatus::Healthy => theme::active().pass,
        SwapStatus::Tight => theme::active().warn,
        SwapStatus::Unknown => theme::active().dim,
    }
}

/// Which of the two stacked tables the operator is scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapPane {
    Cheques,
    Settlements,
}

pub struct Swap {
    rx: watch::Receiver<SwapSnapshot>,
    snapshot: SwapSnapshot,
    market_rx: Option<watch::Receiver<crate::economics_oracle::EconomicsSnapshot>>,
    market: crate::economics_oracle::EconomicsSnapshot,
    focus: SwapPane,
    cheques_offset: usize,
    settlements_offset: usize,
}

impl Swap {
    pub fn new(rx: watch::Receiver<SwapSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self {
            rx,
            snapshot,
            market_rx: None,
            market: crate::economics_oracle::EconomicsSnapshot::default(),
            focus: SwapPane::Cheques,
            cheques_offset: 0,
            settlements_offset: 0,
        }
    }

    pub fn with_market_feed(
        mut self,
        rx: watch::Receiver<crate::economics_oracle::EconomicsSnapshot>,
    ) -> Self {
        self.market = rx.borrow().clone();
        self.market_rx = Some(rx);
        self
    }

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Swap::view_for` call sites resolve.
    pub fn view_for(
        snap: &SwapSnapshot,
        market: Option<&crate::economics_oracle::EconomicsSnapshot>,
    ) -> SwapView {
        view_for(snap, market)
    }

    /// Convenience wrapper for snapshot tests that don't exercise
    /// the Market tile.
    pub fn view_for_no_market(snap: &SwapSnapshot) -> SwapView {
        view_for_no_market(snap)
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        if let Some(rx) = &self.market_rx {
            self.market = rx.borrow().clone();
        }
    }

    fn focused_offset_mut(&mut self) -> &mut usize {
        match self.focus {
            SwapPane::Cheques => &mut self.cheques_offset,
            SwapPane::Settlements => &mut self.settlements_offset,
        }
    }
}

impl Component for Swap {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Left | KeyCode::Char('h') => self.focus = SwapPane::Cheques,
            KeyCode::Right | KeyCode::Char('l') => self.focus = SwapPane::Settlements,
            other => {
                let off = self.focused_offset_mut();
                *off = super::scroll::scroll_key(*off, other);
            }
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let view = view_for(
            &self.snapshot,
            self.market_rx.as_ref().map(|_| &self.market),
        );

        let mut constraints: Vec<Constraint> = vec![Constraint::Length(3)];
        let market_present = view.market.is_some();
        if market_present {
            constraints.push(Constraint::Length(3));
        }
        constraints.push(Constraint::Length(5));
        constraints.push(Constraint::Min(0));
        constraints.push(Constraint::Length(1));
        let chunks = Layout::vertical(constraints).split(area);

        let mut slot = 0usize;
        let header_slot = chunks[slot];
        slot += 1;
        let market_slot = if market_present {
            let s = chunks[slot];
            slot += 1;
            Some(s)
        } else {
            None
        };
        let card_slot = chunks[slot];
        slot += 1;
        let tables_slot = chunks[slot];
        slot += 1;
        let footer_slot = chunks[slot];

        let t = theme::active();
        let mut header_l1 = vec![Span::styled(
            "SWAP / CHEQUES",
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if let Some(addr) = &self.snapshot.chequebook_address {
            header_l1.push(Span::raw("   contract "));
            header_l1.push(Span::styled(addr.clone(), Style::default().fg(t.dim)));
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
            header_slot,
        );

        if let (Some(rect), Some(tile)) = (market_slot, view.market.as_ref()) {
            let prefix = if tile.cold_start {
                format!("{} ", theme::spinner_glyph())
            } else {
                "  ".to_string()
            };
            let mut lines = vec![Line::from(vec![
                Span::raw(prefix.clone()),
                Span::styled("Market  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(tile.price_line.clone()),
                Span::raw("    "),
                Span::raw(tile.gas_line.clone()),
            ])];
            if let Some(why) = &tile.stale_why {
                lines.push(Line::from(vec![
                    Span::raw("    └─ "),
                    Span::styled(
                        format!("stale: {why}"),
                        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
            frame.render_widget(
                Paragraph::new(lines).block(Block::default().borders(Borders::BOTTOM)),
                rect,
            );
        }

        let card = &view.card;
        let mut card_lines = vec![
            Line::from(vec![
                Span::styled(
                    "  Chequebook  ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    card.status.label(),
                    Style::default()
                        .fg(status_color(card.status))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw(format!("    total      {}", card.total)),
                Span::raw("   "),
                Span::raw(format!("available  {}", card.available)),
                Span::raw("   "),
                Span::styled(
                    format!("({}% available)", card.available_pct),
                    Style::default().fg(t.dim),
                ),
            ]),
        ];
        if let Some(why) = &card.why {
            card_lines.push(Line::from(vec![
                Span::raw("    └─ "),
                Span::styled(
                    why.clone(),
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        frame.render_widget(
            Paragraph::new(card_lines).block(Block::default().borders(Borders::BOTTOM)),
            card_slot,
        );

        let table_chunks =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(tables_slot);

        let title_style = |pane: SwapPane| {
            if self.focus == pane {
                Style::default().fg(t.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            }
        };

        let mut cheque_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  PEER          LAST RECEIVED",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if view.cheques.is_empty() {
            cheque_lines.push(Line::from(Span::styled(
                "  (no peer cheques known yet)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            for r in &view.cheques {
                let payout_style = if r.never {
                    Style::default().fg(t.dim)
                } else {
                    Style::default().fg(t.pass)
                };
                cheque_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:<14}", r.peer_short)),
                    Span::styled(r.payout.clone(), payout_style),
                ]));
                cheque_lines.push(Line::from(vec![
                    Span::styled("        peer 0x", Style::default().fg(t.dim)),
                    Span::styled(r.peer_full.clone(), Style::default().fg(t.info)),
                ]));
            }
        }
        let cheques_block = Block::default()
            .borders(Borders::BOTTOM)
            .title(Span::styled(
                " last cheques ",
                title_style(SwapPane::Cheques),
            ));
        let cheques_inner = cheques_block.inner(table_chunks[0]);
        let cheques_visible = cheques_inner.height as usize;
        let cheques_total = cheque_lines.len();
        self.cheques_offset =
            super::scroll::clamp_offset(self.cheques_offset, cheques_visible, cheques_total);
        frame.render_widget(
            Paragraph::new(cheque_lines)
                .block(cheques_block)
                .scroll((self.cheques_offset as u16, 0)),
            table_chunks[0],
        );
        super::scroll::render_scrollbar(
            frame,
            cheques_inner,
            self.cheques_offset,
            cheques_visible,
            cheques_total,
        );

        let mut settle_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  PEER          RECEIVED              SENT                 NET",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if let (Some(tr), Some(ts)) = (&view.time_total_received, &view.time_total_sent) {
            settle_lines.push(Line::from(vec![Span::styled(
                format!("  time-based totals — received {tr} · sent {ts}"),
                Style::default().fg(t.dim),
            )]));
        }
        if view.settlements.is_empty() {
            settle_lines.push(Line::from(Span::styled(
                "  (no peer settlements yet)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            for r in &view.settlements {
                let net_style = if r.net_flagged {
                    Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(t.dim)
                };
                settle_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:<14}", r.peer_short)),
                    Span::raw(format!("{:<22}", r.received)),
                    Span::raw(format!("{:<21}", r.sent)),
                    Span::styled(r.net.clone(), net_style),
                ]));
                settle_lines.push(Line::from(vec![
                    Span::styled("        peer 0x", Style::default().fg(t.dim)),
                    Span::styled(r.peer_full.clone(), Style::default().fg(t.info)),
                ]));
            }
        }
        let settle_block = Block::default().title(Span::styled(
            " settlements ",
            title_style(SwapPane::Settlements),
        ));
        let settle_inner = settle_block.inner(table_chunks[1]);
        let settle_visible = settle_inner.height as usize;
        let settle_total = settle_lines.len();
        self.settlements_offset =
            super::scroll::clamp_offset(self.settlements_offset, settle_visible, settle_total);
        frame.render_widget(
            Paragraph::new(settle_lines)
                .block(settle_block)
                .scroll((self.settlements_offset as u16, 0)),
            table_chunks[1],
        );
        super::scroll::render_scrollbar(
            frame,
            settle_inner,
            self.settlements_offset,
            settle_visible,
            settle_total,
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ←→ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" focus pane  "),
                Span::styled(" ↑↓ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" scroll  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(" net ", Style::default().fg(t.fail)),
                Span::raw(" out-of-balance peer (>0.5 BZZ) "),
            ])),
            footer_slot,
        );

        Ok(())
    }
}
