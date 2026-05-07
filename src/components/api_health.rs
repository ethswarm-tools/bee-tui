//! S8 — RPC / API health screen (`docs/PLAN.md` § 8.S8).
//!
//! PLAN's framing was Gnosis-RPC latency + remote block height, but
//! Bee doesn't expose its eth RPC URL nor a remote chain-tip
//! reference. Pivoting the screen to what we *can* measure:
//!
//! - **Bee API call stats** — latency p50 / p99 + error rate computed
//!   from the same `tracing` capture that powers the S10 command-log
//!   pane. This is the more operator-relevant metric anyway: a slow
//!   Bee API tells you the local node is sluggish, regardless of the
//!   underlying RPC.
//! - **Chain state** — `block` / `chain_tip` / their delta from
//!   `/chainstate`. Bee's own view of the chain.
//! - **Pending operator transactions** — `/transactions` with hash,
//!   nonce, and creation timestamp so a stuck postage-topup or
//!   stake-deposit doesn't disappear into the void.
//!
//! The "Bee doesn't expose its eth RPC URL or remote block height"
//! gap is acknowledged inline so operators see what *isn't*
//! measured rather than assuming silence equals success.
//!
//! Render delegates to the pure [`ApiHealth::view_for`] so the
//! snapshot tests in `tests/s8_api_health_view.rs` pin every
//! statistical edge (empty samples, all-success, mixed errors)
//! without launching a TUI.

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

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::log_capture::{LogCapture, LogEntry};
use crate::theme;
use crate::watch::{HealthSnapshot, TransactionsSnapshot};

/// Window of recent calls considered for the latency / error-rate
/// summary. Tracks the LogCapture's own ring-buffer capacity (200 in
/// `log_capture::install`) — lifting the cap above that just yields
/// the same numbers since older entries are gone.
pub const STATS_WINDOW: usize = 100;

/// Aggregated call statistics over a window of [`LogEntry`] records.
#[derive(Debug, Clone, PartialEq)]
pub struct CallStats {
    /// Number of entries that contributed (had `elapsed_ms` set).
    pub sample_size: usize,
    /// Median latency in milliseconds. `None` if `sample_size == 0`.
    pub p50_ms: Option<u64>,
    /// 99th-percentile latency in milliseconds. `None` if
    /// `sample_size == 0`.
    pub p99_ms: Option<u64>,
    /// Percentage of entries with `status >= 400`. `0.0` when no
    /// entries have a status code attached.
    pub error_rate_pct: f64,
}

/// Bee's view of the chain.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ChainStateView {
    pub block: Option<u64>,
    pub chain_tip: Option<u64>,
    /// `chain_tip - block`, surfaced separately so the renderer can
    /// colour-code it without re-doing the subtraction. Negative
    /// values shouldn't happen on a healthy node but are technically
    /// possible during chain reorgs — the field is signed for that.
    pub delta: Option<i64>,
    pub total_amount: Option<String>,
    pub current_price: Option<String>,
}

/// One row of the pending-transactions table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingTxRow {
    pub nonce: u64,
    pub hash_short: String,
    pub to_short: String,
    /// RFC 3339 creation timestamp, rendered verbatim (no parser is
    /// in scope yet). Empty if Bee didn't supply one.
    pub created: String,
    /// Operator-supplied description from the `description` field.
    /// Empty for system-issued txs.
    pub description: String,
}

/// Aggregated view fed to renderer and snapshot tests.
#[derive(Debug, Clone, PartialEq)]
pub struct ApiHealthView {
    pub bee_endpoint: String,
    pub call_stats: CallStats,
    pub chain: ChainStateView,
    pub pending: Vec<PendingTxRow>,
}

pub struct ApiHealth {
    api: Arc<ApiClient>,
    health_rx: watch::Receiver<HealthSnapshot>,
    transactions_rx: watch::Receiver<TransactionsSnapshot>,
    health: HealthSnapshot,
    transactions: TransactionsSnapshot,
    log_capture: Option<LogCapture>,
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
        }
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.transactions = self.transactions_rx.borrow().clone();
    }

    /// Pure view computation. The log entries arrive as a slice rather
    /// than a `LogCapture` handle so tests can stub deterministic
    /// samples without spinning up the global tracing layer.
    pub fn view_for(
        bee_endpoint: &str,
        recent_calls: &[LogEntry],
        health: &HealthSnapshot,
        transactions: &TransactionsSnapshot,
    ) -> ApiHealthView {
        ApiHealthView {
            bee_endpoint: bee_endpoint.to_string(),
            call_stats: call_stats_for(recent_calls),
            chain: chain_state_view(health),
            pending: pending_rows(transactions),
        }
    }
}

/// Compute call stats over the last [`STATS_WINDOW`] entries that
/// have `elapsed_ms` populated. Latency percentiles are computed via
/// nearest-rank on the sorted sample.
pub fn call_stats_for(entries: &[LogEntry]) -> CallStats {
    let recent: Vec<&LogEntry> = entries.iter().rev().take(STATS_WINDOW).collect();
    let total = recent.len();
    if total == 0 {
        return CallStats {
            sample_size: 0,
            p50_ms: None,
            p99_ms: None,
            error_rate_pct: 0.0,
        };
    }
    let mut latencies: Vec<u64> = recent.iter().filter_map(|e| e.elapsed_ms).collect();
    latencies.sort_unstable();
    let with_latency = latencies.len();
    let p50_ms = percentile(&latencies, 50);
    let p99_ms = percentile(&latencies, 99);
    // Error rate is computed against entries that *do* carry a
    // status — entries without one (in-flight or non-HTTP events)
    // shouldn't pull the rate down.
    let with_status: Vec<u16> = recent.iter().filter_map(|e| e.status).collect();
    let errors = with_status.iter().filter(|s| **s >= 400).count();
    let error_rate_pct = if with_status.is_empty() {
        0.0
    } else {
        (errors as f64) * 100.0 / (with_status.len() as f64)
    };
    CallStats {
        sample_size: with_latency,
        p50_ms,
        p99_ms,
        error_rate_pct,
    }
}

/// Nearest-rank percentile on a pre-sorted slice. Returns `None` for
/// the empty slice. `pct` is in `0..=100`.
fn percentile(sorted: &[u64], pct: u32) -> Option<u64> {
    if sorted.is_empty() {
        return None;
    }
    let n = sorted.len();
    // Nearest-rank: ceil(pct/100 * n) - 1 (clamped to a valid index).
    let rank = (pct as usize * n).div_ceil(100);
    let idx = rank.saturating_sub(1).min(n - 1);
    Some(sorted[idx])
}

fn chain_state_view(health: &HealthSnapshot) -> ChainStateView {
    let Some(cs) = &health.chain_state else {
        return ChainStateView::default();
    };
    let delta = (cs.chain_tip as i64) - (cs.block as i64);
    ChainStateView {
        block: Some(cs.block),
        chain_tip: Some(cs.chain_tip),
        delta: Some(delta),
        total_amount: Some(cs.total_amount.to_string()),
        current_price: Some(cs.current_price.to_string()),
    }
}

fn pending_rows(transactions: &TransactionsSnapshot) -> Vec<PendingTxRow> {
    transactions
        .pending
        .iter()
        .map(|t| PendingTxRow {
            nonce: t.nonce,
            hash_short: short_hex(&t.transaction_hash),
            to_short: short_hex(&t.to),
            created: t.created.clone(),
            description: t.description.clone(),
        })
        .collect()
}

fn short_hex(s: &str) -> String {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.len() > 12 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
    }
}

impl Component for ApiHealth {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
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
        let view = Self::view_for(&self.api.url, &recent, &self.health, &self.transactions);
        let t = theme::active();

        // Header
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

        // Call stats
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

        // Chain state
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

        // Pending tx table
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
                "  NONCE  HASH           TO              CREATED                DESCRIPTION",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )));
            for r in &view.pending {
                pending_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:<6} ", r.nonce)),
                    Span::styled(
                        format!("{:<14} ", r.hash_short),
                        Style::default().fg(t.info),
                    ),
                    Span::raw(format!("{:<15} ", r.to_short)),
                    Span::raw(format!("{:<22} ", truncate(&r.created, 22))),
                    Span::styled(truncate(&r.description, 30), Style::default().fg(t.dim)),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(pending_lines), chunks[3]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
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

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(method: &str, status: Option<u16>, elapsed_ms: Option<u64>) -> LogEntry {
        LogEntry {
            ts: String::new(),
            method: method.into(),
            url: "http://localhost:1633/".into(),
            status,
            elapsed_ms,
            message: String::new(),
        }
    }

    #[test]
    fn call_stats_empty_sample() {
        let stats = call_stats_for(&[]);
        assert_eq!(stats.sample_size, 0);
        assert_eq!(stats.p50_ms, None);
        assert_eq!(stats.p99_ms, None);
        assert_eq!(stats.error_rate_pct, 0.0);
    }

    #[test]
    fn call_stats_all_successful() {
        let entries: Vec<LogEntry> = (1..=100)
            .map(|i| entry("GET", Some(200), Some(i)))
            .collect();
        let stats = call_stats_for(&entries);
        assert_eq!(stats.sample_size, 100);
        assert_eq!(stats.p50_ms, Some(50));
        assert_eq!(stats.p99_ms, Some(99));
        assert_eq!(stats.error_rate_pct, 0.0);
    }

    #[test]
    fn call_stats_mixed_errors() {
        let mut entries: Vec<LogEntry> = (1..=10)
            .map(|i| entry("GET", Some(200), Some(i * 10)))
            .collect();
        entries.push(entry("POST", Some(500), Some(50)));
        entries.push(entry("POST", Some(404), Some(15)));
        let stats = call_stats_for(&entries);
        // 12 entries, 2 errors → 16.67%.
        assert!((stats.error_rate_pct - 16.666_666_666_666_668).abs() < 1e-9);
    }

    #[test]
    fn percentile_single_element() {
        assert_eq!(percentile(&[42], 50), Some(42));
        assert_eq!(percentile(&[42], 99), Some(42));
    }

    #[test]
    fn percentile_empty_returns_none() {
        assert_eq!(percentile(&[], 50), None);
    }

    #[test]
    fn short_hex_truncates_long_address() {
        let s = short_hex("0xabcdef0123456789abcdef0123456789");
        assert!(s.contains('…'));
        assert!(s.starts_with("abcdef"));
    }
}
