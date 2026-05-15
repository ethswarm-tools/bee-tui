//! S8 — RPC / API health screen (`docs/PLAN.md` § 8.S8). Pure view-
//! data half lives in [`bee_cockpit_core::views::api_health`]; this
//! module owns the API client handle, the log-capture subscription,
//! the scroll cursor, and the ratatui draw path.

use std::sync::Arc;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::api_health::{
    ApiHealthView, CallStats, ChainStateView, PENDING_TX_FAIL_AGE_SECS, PENDING_TX_WARN_AGE_SECS,
    PendingTxRow, STATS_WINDOW, call_stats_for, format_age_humanised, parse_rfc3339_to_unix,
    view_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::log_capture::{LogCapture, LogEntry};
use crate::theme;
use crate::watch::{HealthSnapshot, TransactionsSnapshot};

pub struct ApiHealth {
    api: Arc<ApiClient>,
    health_rx: watch::Receiver<HealthSnapshot>,
    transactions_rx: watch::Receiver<TransactionsSnapshot>,
    health: HealthSnapshot,
    transactions: TransactionsSnapshot,
    log_capture: Option<LogCapture>,
    /// Free-scroll offset for the pending-tx table.
    scroll_offset: usize,
}

impl ApiHealth {
    pub fn new(
        api: Arc<ApiClient>,
        health_rx: watch::Receiver<HealthSnapshot>,
        transactions_rx: watch::Receiver<TransactionsSnapshot>,
        log_capture: Option<LogCapture>,
    ) -> Self {
        let health = health_rx.borrow().clone();
        let transactions = transactions_rx.borrow().clone();
        Self {
            api,
            health_rx,
            transactions_rx,
            health,
            transactions,
            log_capture,
            scroll_offset: 0,
        }
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.transactions = self.transactions_rx.borrow().clone();
    }

    /// Re-export of core's pure view computation, kept as an inherent
    /// function so existing `ApiHealth::view_for` call sites resolve.
    pub fn view_for(
        bee_endpoint: &str,
        recent_calls: &[LogEntry],
        health: &HealthSnapshot,
        transactions: &TransactionsSnapshot,
    ) -> ApiHealthView {
        view_for(bee_endpoint, recent_calls, health, transactions)
    }
}

impl Component for ApiHealth {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<Option<Action>> {
        self.scroll_offset = super::scroll::scroll_key(self.scroll_offset, key.code);
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Length(7), // call stats
            Constraint::Length(5), // chain state
            Constraint::Min(0),    // pending tx table
            Constraint::Length(1), // footer
        ])
        .split(area);

        let recent: Vec<LogEntry> = self
            .log_capture
            .as_ref()
            .map(|c| c.snapshot())
            .unwrap_or_default();
        let view = view_for(&self.api.url, &recent, &self.health, &self.transactions);
        let t = theme::active();

        let header_l1 = Line::from(vec![
            Span::styled(
                "RPC / API HEALTH",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   endpoint  "),
            Span::styled(view.bee_endpoint.clone(), Style::default().fg(t.info)),
        ]);
        let header_l2 = Line::from(Span::styled(
            "  Bee doesn't expose its eth RPC URL or remote chain tip; this view measures the local Bee API instead.",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ));
        frame.render_widget(
            Paragraph::new(vec![header_l1, header_l2])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let cs = &view.call_stats;
        let p50 = cs
            .p50_ms
            .map(|v| format!("{v} ms"))
            .unwrap_or_else(|| "—".into());
        let p99 = cs
            .p99_ms
            .map(|v| format!("{v} ms"))
            .unwrap_or_else(|| "—".into());
        let err_color = if cs.error_rate_pct >= 5.0 {
            t.fail
        } else if cs.error_rate_pct >= 1.0 {
            t.warn
        } else {
            t.pass
        };
        let stats_lines = vec![
            Line::from(vec![Span::styled(
                "  CALL STATS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![
                Span::raw("    p50 latency   "),
                Span::styled(p50, Style::default().fg(t.pass)),
            ]),
            Line::from(vec![
                Span::raw("    p99 latency   "),
                Span::styled(p99, Style::default().fg(t.warn)),
            ]),
            Line::from(vec![
                Span::raw("    error rate    "),
                Span::styled(
                    format!("{:.2}%", cs.error_rate_pct),
                    Style::default().fg(err_color).add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw("    sample size   "),
                Span::styled(
                    format!("{} call(s) (last {STATS_WINDOW})", cs.sample_size),
                    Style::default().fg(t.dim),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(stats_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        let block_str = view
            .chain
            .block
            .map(|b| b.to_string())
            .unwrap_or_else(|| "—".into());
        let tip_str = view
            .chain
            .chain_tip
            .map(|b| b.to_string())
            .unwrap_or_else(|| "—".into());
        let delta_str = view
            .chain
            .delta
            .map(|d| format!("{d:+}"))
            .unwrap_or_else(|| "—".into());
        let chain_lines = vec![
            Line::from(vec![Span::styled(
                "  CHAIN STATE  (Bee's view, not the wider network)",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )]),
            Line::from(vec![
                Span::raw("    block "),
                Span::styled(block_str, Style::default().fg(t.pass)),
                Span::raw("   chain tip "),
                Span::styled(tip_str, Style::default().fg(t.pass)),
                Span::raw("   Δ "),
                Span::styled(delta_str, Style::default().fg(t.warn)),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(chain_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[2],
        );

        let mut pending_lines = vec![Line::from(Span::styled(
            format!("  PENDING TRANSACTIONS  ({})", view.pending.len()),
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if view.pending.is_empty() {
            pending_lines.push(Line::from(Span::styled(
                "  (no pending operator transactions — all confirmed)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            pending_lines.push(Line::from(Span::styled(
                "  NONCE  HASH           TO              AGE        DESCRIPTION",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )));
            for r in &view.pending {
                let age_str = format_age_humanised(r.age_seconds);
                let age_style = match r.age_seconds {
                    Some(s) if s >= PENDING_TX_FAIL_AGE_SECS => {
                        Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                    }
                    Some(s) if s >= PENDING_TX_WARN_AGE_SECS => Style::default().fg(t.warn),
                    _ => Style::default(),
                };
                pending_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:<6} ", r.nonce)),
                    Span::styled(
                        format!("{:<14} ", r.hash_short),
                        Style::default().fg(t.info),
                    ),
                    Span::raw(format!("{:<15} ", r.to_short)),
                    Span::styled(format!("{age_str:<10} "), age_style),
                    Span::styled(truncate(&r.description, 30), Style::default().fg(t.dim)),
                ]));
                pending_lines.push(Line::from(vec![
                    Span::styled("        hash 0x", Style::default().fg(t.dim)),
                    Span::styled(r.hash_full.clone(), Style::default().fg(t.info)),
                    Span::styled("  to 0x", Style::default().fg(t.dim)),
                    Span::styled(r.to_full.clone(), Style::default().fg(t.info)),
                ]));
            }
            pending_lines.push(Line::from(Span::styled(
                format!(
                    "  └─ age >= {}m colours warn; >= {}m colours fail (likely under-priced gas)",
                    PENDING_TX_WARN_AGE_SECS / 60,
                    PENDING_TX_FAIL_AGE_SECS / 60
                ),
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        let pending_area = chunks[3];
        let visible_rows = pending_area.height as usize;
        let pending_total = pending_lines.len();
        self.scroll_offset =
            super::scroll::clamp_offset(self.scroll_offset, visible_rows, pending_total);
        frame.render_widget(
            Paragraph::new(pending_lines).scroll((self.scroll_offset as u16, 0)),
            pending_area,
        );
        super::scroll::render_scrollbar(
            frame,
            pending_area,
            self.scroll_offset,
            visible_rows,
            pending_total,
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" scroll  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(
                    "stats live-update from S10's command-log capture",
                    Style::default().fg(t.dim),
                ),
            ])),
            chunks[4],
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
