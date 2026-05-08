//! S3 — SWAP / cheques screen (`docs/PLAN.md` § 8.S3).
//!
//! Three stacked panes driven by [`crate::watch::SwapSnapshot`]:
//!
//! 1. **Chequebook balance card** — total + available, with the
//!    headroom (`available / total`) called out so operators notice
//!    when they're approaching cashout pressure.
//! 2. **Last received cheques** (per peer) — sorted by cumulative
//!    payout descending so the most lucrative peers float to the top.
//!    A peer with no cheque yet is rendered as a quiet "—" row instead
//!    of being hidden, because "no cheques" is itself useful signal.
//! 3. **Per-peer settlements table** — received and sent PLUR side by
//!    side, with the net (received − sent) flagged. SWAP keeps these
//!    near zero in steady state; a large net delta means cheques are
//!    being issued faster than they're being cashed.
//!
//! Render path delegates to the pure [`Swap::view_for`] function so
//! `tests/s3_swap_view.rs` can pin all the formatting (PLUR scaling,
//! sort order, net-balance highlighting) without launching a TUI.

use color_eyre::Result;
use num_bigint::BigInt;
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
use crate::watch::SwapSnapshot;

use bee::debug::{ChequebookBalance, LastCheque, Settlement, Settlements};

/// Tri-state outcome for the chequebook balance card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwapStatus {
    /// `total_balance` is zero — chequebook hasn't been funded yet.
    Empty,
    /// Available balance is healthy (>20 % of total).
    Healthy,
    /// Available drops below 20 % of total — most funds are tied up
    /// in unsettled debt and cashing out is getting close.
    Tight,
    /// `/chequebook/balance` failed; nothing to render.
    Unknown,
}

impl SwapStatus {
    fn color(self) -> Color {
        match self {
            Self::Empty => theme::active().warn,
            Self::Healthy => theme::active().pass,
            Self::Tight => theme::active().warn,
            Self::Unknown => theme::active().dim,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Empty => "○ unfunded",
            Self::Healthy => "✓ healthy",
            Self::Tight => "⚠ tight",
            Self::Unknown => "? unknown",
        }
    }
}

/// Snapshot-friendly view of the chequebook card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChequebookCard {
    pub status: SwapStatus,
    /// PLUR formatted as `BZZ x.xxxx` (BZZ has 16 decimals).
    pub total: String,
    pub available: String,
    /// `available / total` as 0..=100. `0` if total is zero.
    pub available_pct: u32,
    pub why: Option<String>,
}

/// One row of the "last received cheques" pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckRow {
    pub peer_short: String,
    /// Pre-formatted payout (`BZZ x.xxxx` or `—` if no cheque yet).
    pub payout: String,
    /// `true` if this peer has not sent us any cheque yet.
    pub never: bool,
}

/// One row of the per-peer settlements pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SettlementRow {
    pub peer_short: String,
    pub received: String,
    pub sent: String,
    /// Sign-prefixed net (`+x` if we're owed, `-x` if we owe).
    pub net: String,
    /// `true` when |net| > 0.5 BZZ — flagged in red.
    pub net_flagged: bool,
}

/// Aggregated view fed to both the renderer and snapshot tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SwapView {
    pub card: ChequebookCard,
    /// On-chain chequebook contract address from
    /// `/chequebook/address`. `None` when the endpoint hasn't
    /// returned yet (cold start) — the header silently omits the
    /// row in that case rather than rendering a placeholder.
    pub chequebook_address: Option<String>,
    pub cheques: Vec<CheckRow>,
    pub settlements: Vec<SettlementRow>,
    pub time_total_received: Option<String>,
    pub time_total_sent: Option<String>,
    /// Pre-rendered Market tile lines. `None` when the operator
    /// hasn't enabled `[economics].enable_market_tile` — the
    /// renderer skips the tile row entirely in that case.
    pub market: Option<MarketTile>,
}

/// Snapshot of the Market tile lines. Pure data so snapshot tests
/// can pin the formatted strings without poking the renderer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarketTile {
    /// `BZZ ≈ $0.4321` — `—` when price hasn't loaded yet.
    pub price_line: String,
    /// `gas: 1.20 base + 0.50 tip = 1.70 gwei` — `—` when not
    /// configured or basefee fetch failed.
    pub gas_line: String,
    /// "stale: ..." line shown dim+italic under the tile when the
    /// last poll surfaced an error. `None` when the latest poll
    /// was clean, or when no poll has run yet (cold start shows a
    /// `loading…` line via `cold_start` instead).
    pub stale_why: Option<String>,
    /// `true` while no poll has completed — drives the spinner
    /// glyph in the tile so cold-start motion is visible.
    pub cold_start: bool,
}

pub struct Swap {
    rx: watch::Receiver<SwapSnapshot>,
    snapshot: SwapSnapshot,
    /// Optional cost-context feed driving the Market tile. `None`
    /// when `[economics].enable_market_tile` is off — the tile is
    /// suppressed entirely in that case so the layout doesn't waste
    /// a row on placeholder text.
    market_rx: Option<watch::Receiver<crate::economics_oracle::EconomicsSnapshot>>,
    market: crate::economics_oracle::EconomicsSnapshot,
}

impl Swap {
    pub fn new(rx: watch::Receiver<SwapSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self {
            rx,
            snapshot,
            market_rx: None,
            market: crate::economics_oracle::EconomicsSnapshot::default(),
        }
    }

    /// Builder-style attach for the cost-context feed. Call once
    /// after `new` when `[economics].enable_market_tile` is true.
    pub fn with_market_feed(
        mut self,
        rx: watch::Receiver<crate::economics_oracle::EconomicsSnapshot>,
    ) -> Self {
        self.market = rx.borrow().clone();
        self.market_rx = Some(rx);
        self
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        if let Some(rx) = &self.market_rx {
            self.market = rx.borrow().clone();
        }
    }

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests in `tests/s3_swap_view.rs`. The `market` argument is
    /// `None` when `[economics].enable_market_tile` is off — the
    /// tile is suppressed entirely in that case so cold-start
    /// renders identically to today.
    pub fn view_for(
        snap: &SwapSnapshot,
        market: Option<&crate::economics_oracle::EconomicsSnapshot>,
    ) -> SwapView {
        let card = card_for(snap.chequebook.as_ref());
        let cheques = cheque_rows_for(&snap.last_received);
        let settlements = settlement_rows_for(snap.settlements.as_ref());
        let time_total_received = snap
            .time_settlements
            .as_ref()
            .and_then(|s| s.total_received.as_ref())
            .map(format_plur);
        let time_total_sent = snap
            .time_settlements
            .as_ref()
            .and_then(|s| s.total_sent.as_ref())
            .map(format_plur);
        let market = market.map(market_tile_for);
        SwapView {
            card,
            chequebook_address: snap.chequebook_address.clone(),
            cheques,
            settlements,
            time_total_received,
            time_total_sent,
            market,
        }
    }

    /// Convenience wrapper used by snapshot tests that don't care
    /// about the optional Market tile. Equivalent to
    /// `view_for(snap, None)`.
    pub fn view_for_no_market(snap: &SwapSnapshot) -> SwapView {
        Self::view_for(snap, None)
    }
}

/// Pure formatter — exposed for tests. Builds the four tile lines
/// from the latest poller snapshot.
fn market_tile_for(m: &crate::economics_oracle::EconomicsSnapshot) -> MarketTile {
    let price_line = match &m.price {
        Some(p) => format!("BZZ ≈ ${:.4}", p.usd),
        None => "BZZ ≈ —".to_string(),
    };
    let gas_line = match &m.gas {
        Some(g) => match g.max_priority_fee_gwei {
            Some(tip) => format!(
                "gas: {:.2} base + {:.2} tip = {:.2} gwei",
                g.base_fee_gwei,
                tip,
                g.total_gwei(),
            ),
            None => format!("gas: {:.2} gwei base", g.base_fee_gwei),
        },
        None => "gas: —".to_string(),
    };
    MarketTile {
        price_line,
        gas_line,
        stale_why: m.last_error.clone(),
        cold_start: m.last_polled.is_none(),
    }
}

fn card_for(cb: Option<&ChequebookBalance>) -> ChequebookCard {
    let Some(cb) = cb else {
        return ChequebookCard {
            status: SwapStatus::Unknown,
            total: "—".into(),
            available: "—".into(),
            available_pct: 0,
            why: Some("/chequebook/balance not available yet".into()),
        };
    };
    let zero = BigInt::from(0);
    let total = &cb.total_balance;
    let avail = &cb.available_balance;
    let total_str = format_plur(total);
    let avail_str = format_plur(avail);
    if total == &zero {
        return ChequebookCard {
            status: SwapStatus::Empty,
            total: total_str,
            available: avail_str,
            available_pct: 0,
            why: Some("chequebook holds 0 BZZ — fund it to send cheques.".into()),
        };
    }
    let pct = pct_of(avail, total);
    let (status, why) = if pct < 20 {
        (
            SwapStatus::Tight,
            Some(format!(
                "only {pct}% available — most BZZ is tied up in unsettled debt."
            )),
        )
    } else {
        (SwapStatus::Healthy, None)
    };
    ChequebookCard {
        status,
        total: total_str,
        available: avail_str,
        available_pct: pct,
        why,
    }
}

fn cheque_rows_for(last_received: &[LastCheque]) -> Vec<CheckRow> {
    let mut rows: Vec<CheckRow> = last_received
        .iter()
        .map(|lc| {
            let payout_bi = lc.last_received.as_ref().and_then(|c| c.payout.as_ref());
            let (payout, never) = match payout_bi {
                Some(p) => (format_plur(p), false),
                None => ("—".into(), true),
            };
            CheckRow {
                peer_short: short_peer(&lc.peer),
                payout,
                never,
            }
        })
        .collect();
    // Sort: peers with cheques first, by payout descending (string-sort is OK
    // because format_plur produces fixed-decimal `BZZ x.xxxx` form). Peers with
    // no cheque sink to the bottom but stay visible.
    rows.sort_by(|a, b| match (a.never, b.never) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => b.payout.cmp(&a.payout),
    });
    rows
}

fn settlement_rows_for(s: Option<&Settlements>) -> Vec<SettlementRow> {
    let Some(s) = s else { return Vec::new() };
    // Sort the raw settlements by |received - sent| descending so the
    // most out-of-balance peers float to the top — that's where cashout
    // pressure shows up first.
    let mut sorted: Vec<&Settlement> = s.settlements.iter().collect();
    sorted.sort_by_key(|s| std::cmp::Reverse(abs_net(s)));
    sorted.into_iter().map(settlement_row).collect()
}

fn abs_net(s: &Settlement) -> BigInt {
    let zero = BigInt::from(0);
    let recv = s.received.as_ref().unwrap_or(&zero);
    let sent = s.sent.as_ref().unwrap_or(&zero);
    let net = recv - sent;
    if net < zero { -net } else { net }
}

fn settlement_row(s: &Settlement) -> SettlementRow {
    let zero = BigInt::from(0);
    let recv = s.received.as_ref().unwrap_or(&zero);
    let sent = s.sent.as_ref().unwrap_or(&zero);
    let net_bi = recv - sent;
    let net = format_plur_signed(&net_bi);
    // Flag peers >0.5 BZZ out of balance (5 * 10^15 PLUR).
    let half_bzz = BigInt::from(5_000_000_000_000_000u64);
    let abs = if net_bi < BigInt::from(0) {
        -net_bi
    } else {
        net_bi
    };
    let net_flagged = abs > half_bzz;
    SettlementRow {
        peer_short: short_peer(&s.peer),
        received: format_plur(recv),
        sent: format_plur(sent),
        net,
        net_flagged,
    }
}

/// Format a PLUR amount as `BZZ x.xxxx` with 4 fractional digits.
/// Bee's PLUR has 16 decimals; we render the top 4 digits of the
/// fractional part so 0.0001 BZZ is the smallest visible unit, which
/// is finer than any realistic per-cheque amount.
pub fn format_plur(plur: &BigInt) -> String {
    format_plur_inner(plur, false)
}

fn format_plur_signed(plur: &BigInt) -> String {
    format_plur_inner(plur, true)
}

fn format_plur_inner(plur: &BigInt, signed: bool) -> String {
    let zero = BigInt::from(0);
    let neg = plur < &zero;
    let abs = if neg { -plur.clone() } else { plur.clone() };
    let scale = BigInt::from(10u64).pow(16);
    let whole = &abs / &scale;
    let frac = &abs % &scale;
    // Take top 4 digits: divide by 10^12.
    let frac_4 = &frac / BigInt::from(10u64).pow(12);
    let sign = if neg {
        "-"
    } else if signed {
        "+"
    } else {
        ""
    };
    format!("{sign}BZZ {whole}.{frac_4:0>4}")
}

fn pct_of(num: &BigInt, denom: &BigInt) -> u32 {
    let zero = BigInt::from(0);
    if denom == &zero {
        return 0;
    }
    // (num * 100) / denom, clamped to 100.
    let scaled = num * BigInt::from(100);
    let q = &scaled / denom;
    // Convert to u32 best-effort; in practice 0..=100.
    let q_str = q.to_string();
    let q_u: u128 = q_str.parse().unwrap_or(0);
    q_u.min(100) as u32
}

fn short_peer(p: &str) -> String {
    let trimmed = p.trim_start_matches("0x");
    if trimmed.len() > 10 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
    }
}

impl Component for Swap {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let view = Self::view_for(
            &self.snapshot,
            self.market_rx.as_ref().map(|_| &self.market),
        );

        // Layout slots: header, optional market tile (3 rows when
        // present), chequebook card, tables, footer. Computing slots
        // up-front keeps the conditional in one place.
        let mut constraints: Vec<Constraint> = vec![Constraint::Length(3)];
        let market_present = view.market.is_some();
        if market_present {
            constraints.push(Constraint::Length(3));
        }
        constraints.push(Constraint::Length(5)); // chequebook card
        constraints.push(Constraint::Min(0)); // tables
        constraints.push(Constraint::Length(1)); // footer
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
        // Header
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

        // Market tile (xBZZ price + Gnosis basefee). Rendered only
        // when [economics].enable_market_tile is on. Two-line tile:
        // price + gas, with optional dim "stale" why-line under it.
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

        // Chequebook card
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
                        .fg(card.status.color())
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

        // Tables stacked: cheques (top) + settlements (bottom)
        let table_chunks =
            Layout::vertical([Constraint::Percentage(40), Constraint::Percentage(60)])
                .split(tables_slot);

        // Cheques table
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
            }
        }
        frame.render_widget(
            Paragraph::new(cheque_lines).block(Block::default().borders(Borders::BOTTOM).title(
                Span::styled(
                    " last cheques ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            )),
            table_chunks[0],
        );

        // Settlements table
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
            }
        }
        frame.render_widget(
            Paragraph::new(settle_lines).block(Block::default().title(Span::styled(
                " settlements ",
                Style::default().add_modifier(Modifier::BOLD),
            ))),
            table_chunks[1],
        );

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_plur_zero() {
        assert_eq!(format_plur(&BigInt::from(0)), "BZZ 0.0000");
    }

    #[test]
    fn format_plur_one_bzz() {
        let one = BigInt::from(10u64).pow(16);
        assert_eq!(format_plur(&one), "BZZ 1.0000");
    }

    #[test]
    fn format_plur_fractional() {
        // 0.5 BZZ = 5 * 10^15 PLUR
        let half = BigInt::from(5_000_000_000_000_000u64);
        assert_eq!(format_plur(&half), "BZZ 0.5000");
    }

    #[test]
    fn format_plur_signed_negative() {
        let one = BigInt::from(10u64).pow(16);
        assert_eq!(format_plur_signed(&-one), "-BZZ 1.0000");
    }

    #[test]
    fn format_plur_signed_positive() {
        let one = BigInt::from(10u64).pow(16);
        assert_eq!(format_plur_signed(&one), "+BZZ 1.0000");
    }

    #[test]
    fn pct_of_handles_zero_denom() {
        assert_eq!(pct_of(&BigInt::from(10), &BigInt::from(0)), 0);
    }

    #[test]
    fn pct_of_clamps_to_100() {
        assert_eq!(pct_of(&BigInt::from(200), &BigInt::from(100)), 100);
    }

    #[test]
    fn short_peer_truncates_long_overlay() {
        let p = "0xabcdef0123456789abcdef0123456789";
        let s = short_peer(p);
        assert!(s.contains('…'));
        assert!(s.starts_with("abcdef"));
    }

    #[test]
    fn short_peer_passes_short_through() {
        assert_eq!(short_peer("abcd"), "abcd");
    }
}
