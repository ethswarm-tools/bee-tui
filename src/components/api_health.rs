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
    /// Full transaction hash (with the `0x` prefix stripped). Rendered
    /// on the row's continuation line so operators can click-drag to
    /// copy without losing the table layout.
    pub hash_full: String,
    /// Full destination address (`0x` stripped). Same rationale as
    /// `hash_full`.
    pub to_full: String,
    /// RFC 3339 creation timestamp, rendered verbatim. Empty if Bee
    /// didn't supply one (very early Bee builds).
    pub created: String,
    /// Operator-supplied description from the `description` field.
    /// Empty for system-issued txs.
    pub description: String,
    /// Seconds elapsed since `created`. `None` when the timestamp
    /// failed to parse (or was empty). The renderer humanises this
    /// into `5s` / `2m 30s` / `8h 15m` and colour-codes by threshold:
    /// stuck transactions are the most operator-relevant signal in
    /// this whole pane (a 10-minute-old pending topup is almost
    /// always under-priced gas, not Bee being slow).
    pub age_seconds: Option<i64>,
}

/// Pending-tx age threshold above which the row colours warn-yellow.
/// 5 minutes — short enough that operators still see colour during a
/// normal Gnosis confirmation cycle (~10s/block, 6+ blocks for
/// finality), long enough that the threshold doesn't fire on every
/// healthy submission.
pub const PENDING_TX_WARN_AGE_SECS: i64 = 300;
/// Above this the row colours fail-red — at this point the operator
/// almost certainly needs to bump gas / cancel.
pub const PENDING_TX_FAIL_AGE_SECS: i64 = 1800;

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
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    transactions
        .pending
        .iter()
        .map(|t| {
            let age_seconds = parse_rfc3339_to_unix(&t.created).map(|ts| now_unix - ts);
            PendingTxRow {
                nonce: t.nonce,
                hash_short: short_hex(&t.transaction_hash),
                to_short: short_hex(&t.to),
                hash_full: t.transaction_hash.trim_start_matches("0x").to_string(),
                to_full: t.to.trim_start_matches("0x").to_string(),
                created: t.created.clone(),
                description: t.description.clone(),
                age_seconds,
            }
        })
        .collect()
}

/// Parse Bee's RFC 3339 timestamp (`"2026-05-07T08:12:03Z"` or
/// `"2026-05-07T08:12:03+00:00"`) into seconds-since-Unix-epoch.
/// Returns `None` for malformed / empty input — the caller falls
/// back to a `—` in the age column rather than guessing.
pub fn parse_rfc3339_to_unix(s: &str) -> Option<i64> {
    if s.is_empty() {
        return None;
    }
    time::OffsetDateTime::parse(s, &time::format_description::well_known::Rfc3339)
        .ok()
        .map(|odt| odt.unix_timestamp())
}

/// Humanise `age_seconds` into `5s` / `2m 30s` / `8h 15m`. Negative
/// values (clock skew on the host) collapse to `now`. Returns `—`
/// for `None` so the renderer doesn't have to special-case the
/// missing-timestamp path.
pub fn format_age_humanised(age_seconds: Option<i64>) -> String {
    match age_seconds {
        None => "—".into(),
        Some(s) if s < 0 => "now".into(),
        Some(s) if s < 60 => format!("{s}s"),
        Some(s) if s < 3_600 => {
            let m = s / 60;
            let r = s % 60;
            format!("{m}m {r:>2}s")
        }
        Some(s) => {
            let h = s / 3_600;
            let m = (s % 3_600) / 60;
            format!("{h}h {m:>2}m")
        }
    }
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
                // Continuation line with full hash + to-address so
                // operators can click-drag to copy. The columns above
                // stay short to preserve the table layout.
                pending_lines.push(Line::from(vec![
                    Span::styled("        hash 0x", Style::default().fg(t.dim)),
                    Span::styled(r.hash_full.clone(), Style::default().fg(t.info)),
                    Span::styled("  to 0x", Style::default().fg(t.dim)),
                    Span::styled(r.to_full.clone(), Style::default().fg(t.info)),
                ]));
            }
            // Tooltip line — operators new to the screen don't know
            // what the colour means or where the threshold sits.
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

        // Footer
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
    fn parse_rfc3339_z_form() {
        // Bee's most common format — Z suffix, second precision.
        let ts = parse_rfc3339_to_unix("2026-05-07T08:12:03Z").expect("must parse");
        assert!(ts > 1_700_000_000); // sanity: way past 2023
    }

    #[test]
    fn parse_rfc3339_offset_form() {
        let ts = parse_rfc3339_to_unix("2026-05-07T08:12:03+00:00").expect("must parse");
        assert!(ts > 1_700_000_000);
    }

    #[test]
    fn parse_rfc3339_returns_none_on_garbage() {
        assert_eq!(parse_rfc3339_to_unix(""), None);
        assert_eq!(parse_rfc3339_to_unix("not a date"), None);
        assert_eq!(parse_rfc3339_to_unix("2026"), None);
    }

    #[test]
    fn format_age_humanised_seconds() {
        assert_eq!(format_age_humanised(Some(0)), "0s");
        assert_eq!(format_age_humanised(Some(45)), "45s");
        assert_eq!(format_age_humanised(Some(59)), "59s");
    }

    #[test]
    fn format_age_humanised_minutes() {
        assert_eq!(format_age_humanised(Some(60)), "1m  0s");
        assert_eq!(format_age_humanised(Some(125)), "2m  5s");
        assert_eq!(format_age_humanised(Some(3_599)), "59m 59s");
    }

    #[test]
    fn format_age_humanised_hours() {
        assert_eq!(format_age_humanised(Some(3_600)), "1h  0m");
        assert_eq!(format_age_humanised(Some(8 * 3_600 + 15 * 60)), "8h 15m");
    }

    #[test]
    fn format_age_humanised_special_cases() {
        assert_eq!(format_age_humanised(None), "—");
        // Negative = clock skew (host's clock is ahead of Bee's).
        // Treat as "now" rather than render "-3s".
        assert_eq!(format_age_humanised(Some(-3)), "now");
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
