//! S2 — Stamps screen (`docs/PLAN.md` § 8.S2).
//!
//! Renders one row per known postage batch with the volume +
//! duration framing operators actually reason about (bee#4992 is
//! retiring depth+amount). The "worst bucket" column tells the
//! truth that the API's `utilization` field is `MaxBucketCount`
//! — operators see exactly which batch is about to fail uploads
//! even though average usage is far from 100%.
//!
//! Behaviour is data-driven via [`Stamps::rows_for`] so insta
//! snapshot tests can stub the input and verify status / value /
//! `why` strings without launching a TUI.

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
use crate::watch::StampsSnapshot;

use bee::postage::PostageBatch;

/// Tri-state row outcome with `Pending` for chain-confirmation gating.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StampStatus {
    /// `usable=false` — chain hasn't confirmed the batch yet.
    Pending,
    /// `batch_ttl ≤ 0` — paid balance is exhausted, nothing to stamp.
    Expired,
    /// Worst bucket ≥ 95 % — the very next upload may fail
    /// (immutable batches return ErrBucketFull at the upper bound).
    Critical,
    /// Worst bucket ≥ 80 %. Above the safe-headroom line; warn early.
    Skewed,
    /// Everything green: usable, in budget, headroom present.
    Healthy,
}

impl StampStatus {
    fn color(self) -> Color {
        let t = theme::active();
        match self {
            Self::Pending => t.info,
            Self::Expired => t.fail,
            Self::Critical => t.fail,
            Self::Skewed => t.warn,
            Self::Healthy => t.pass,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Pending => "⏳ pending",
            Self::Expired => "✗ expired",
            Self::Critical => "✗ critical",
            Self::Skewed => "⚠ skewed",
            Self::Healthy => "✓",
        }
    }
}

/// One row of the stamps table.
#[derive(Debug, Clone)]
pub struct StampRow {
    pub label: String,
    pub batch_id_short: String,
    /// Theoretical volume = `2^depth × 4 KiB`, formatted to a
    /// human-readable string. Effective volume is bounded by the
    /// worst bucket — see `worst_bucket_pct`.
    pub volume: String,
    /// Worst-bucket fill percentage in `0..=100`. This *is* what the
    /// API calls `utilization`; operators don't always realise.
    pub worst_bucket_pct: u32,
    /// Raw `utilization` count plus `BucketUpperBound`.
    pub worst_bucket_raw: String,
    /// Pre-formatted `Xd Yh` countdown string. `"-"` if expired.
    pub ttl: String,
    /// `true` if `immutable` — flagged in the `value` line because
    /// mutable + full silently overwrites prior chunks (bee#5334).
    pub immutable: bool,
    pub status: StampStatus,
    /// Inline tooltip rendered on the continuation line.
    pub why: Option<String>,
}

pub struct Stamps {
    rx: watch::Receiver<StampsSnapshot>,
    snapshot: StampsSnapshot,
}

impl Stamps {
    pub fn new(rx: watch::Receiver<StampsSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self { rx, snapshot }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
    }

    /// Pure, snapshot-driven row computation. Exposed for snapshot
    /// tests.
    pub fn rows_for(snap: &StampsSnapshot) -> Vec<StampRow> {
        snap.batches.iter().map(row_from_batch).collect()
    }
}

fn row_from_batch(b: &PostageBatch) -> StampRow {
    let label = if b.label.is_empty() {
        "(unlabeled)".to_string()
    } else {
        b.label.clone()
    };
    let batch_hex = b.batch_id.to_hex();
    let batch_id_short = if batch_hex.len() > 8 {
        format!("{}…", &batch_hex[..8])
    } else {
        batch_hex
    };
    let theoretical_bytes: u128 = (1u128 << b.depth) * 4096;
    let volume = format_bytes(theoretical_bytes);
    let worst_bucket_pct = worst_bucket_pct(b);
    let upper_bound = 1u32 << b.depth.saturating_sub(b.bucket_depth);
    let worst_bucket_raw = format!("{}/{}", b.utilization, upper_bound);
    let ttl = format_ttl_seconds(b.batch_ttl);

    let (status, why) = if !b.usable {
        (
            StampStatus::Pending,
            Some("waiting on chain confirmation (~10 blocks).".into()),
        )
    } else if b.batch_ttl <= 0 {
        (
            StampStatus::Expired,
            Some("paid balance exhausted; topup or stop using.".into()),
        )
    } else if worst_bucket_pct >= 95 {
        (
            StampStatus::Critical,
            Some(if b.immutable {
                "immutable batch will REJECT next upload at this bucket.".into()
            } else {
                "mutable batch will silently overwrite oldest chunks.".into()
            }),
        )
    } else if worst_bucket_pct >= 80 {
        (
            StampStatus::Skewed,
            Some(format!(
                "worst bucket {worst_bucket_pct}% > safe headroom — dilute or stop using."
            )),
        )
    } else {
        (StampStatus::Healthy, None)
    };

    StampRow {
        label,
        batch_id_short,
        volume,
        worst_bucket_pct,
        worst_bucket_raw,
        ttl,
        immutable: b.immutable,
        status,
        why,
    }
}

/// `MaxBucketCount` (Bee's `utilization`) as a 0..=100 percentage of
/// the per-bucket upper bound `2^(depth - bucket_depth)`.
fn worst_bucket_pct(b: &PostageBatch) -> u32 {
    let upper_bound: u32 = 1u32 << b.depth.saturating_sub(b.bucket_depth);
    if upper_bound == 0 {
        0
    } else {
        let pct = (u64::from(b.utilization) * 100) / u64::from(upper_bound);
        pct.min(100) as u32
    }
}

/// Bytes → IEC binary (KiB / MiB / GiB / TiB).
fn format_bytes(bytes: u128) -> String {
    const K: u128 = 1024;
    const M: u128 = K * 1024;
    const G: u128 = M * 1024;
    const T: u128 = G * 1024;
    if bytes >= T {
        format!("{:.1} TiB", bytes as f64 / T as f64)
    } else if bytes >= G {
        format!("{:.1} GiB", bytes as f64 / G as f64)
    } else if bytes >= M {
        format!("{:.1} MiB", bytes as f64 / M as f64)
    } else if bytes >= K {
        format!("{:.1} KiB", bytes as f64 / K as f64)
    } else {
        format!("{bytes} B")
    }
}

fn format_ttl_seconds(secs: i64) -> String {
    if secs <= 0 {
        return "expired".into();
    }
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    if days >= 1 {
        format!("{days}d {hours:>2}h")
    } else {
        let minutes = (secs % 3_600) / 60;
        format!("{hours}h {minutes:>2}m")
    }
}

/// 8-character ASCII fill bar.
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

        // Header
        let count = self.snapshot.batches.len();
        let header_l1 = Line::from(vec![
            Span::styled("STAMPS", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw(format!("  {count} batch(es)")),
        ]);
        let mut header_l2 = Vec::new();
        let t = theme::active();
        if let Some(err) = &self.snapshot.last_error {
            let (color, msg) = theme::classify_header_error(err);
            header_l2.push(Span::styled(msg, Style::default().fg(color)));
        } else if !self.snapshot.is_loaded() {
            header_l2.push(Span::styled(
                "loading…",
                Style::default().fg(t.dim),
            ));
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Table (rendered as a Paragraph of styled Lines for control)
        let mut lines: Vec<Line> = Vec::new();
        // Column header
        lines.push(Line::from(vec![Span::styled(
            "  LABEL                BATCH        VOLUME      WORST BUCKET                TTL         STATUS",
            Style::default()
                .fg(t.dim)
                .add_modifier(Modifier::BOLD),
        )]));
        if self.snapshot.batches.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no batches yet — buy one with swarm-cli or `bee stamps buy`)",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            for r in Self::rows_for(&self.snapshot) {
                let bar = fill_bar(r.worst_bucket_pct, 8);
                let immut_glyph = if r.immutable { "I" } else { "M" };
                lines.push(Line::from(vec![
                    Span::raw("  "),
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
                            .fg(r.status.color())
                            .add_modifier(Modifier::BOLD),
                    ),
                ]));
                if let Some(why) = r.why {
                    lines.push(Line::from(vec![
                        Span::raw("       └─ "),
                        Span::styled(
                            why,
                            Style::default()
                                .fg(t.dim)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }
        }
        frame.render_widget(Paragraph::new(lines), chunks[1]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(" I/M ", Style::default().fg(t.dim)),
                Span::raw(" immutable / mutable "),
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
        assert_eq!(fill_bar(150, 8), "▇▇▇▇▇▇▇▇"); // saturating
    }

    #[test]
    fn format_bytes_iec() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(1024), "1.0 KiB");
        assert_eq!(format_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(format_bytes(1024 * 1024 * 1024), "1.0 GiB");
        assert_eq!(format_bytes(16 * 1024 * 1024 * 1024), "16.0 GiB");
    }

    #[test]
    fn format_ttl_zero_is_expired() {
        assert_eq!(format_ttl_seconds(0), "expired");
        assert_eq!(format_ttl_seconds(-5), "expired");
    }

    #[test]
    fn format_ttl_days_and_hours() {
        // 47d 12h
        assert_eq!(format_ttl_seconds(47 * 86_400 + 12 * 3_600), "47d 12h");
    }

    #[test]
    fn format_ttl_under_a_day_uses_hours_minutes() {
        assert_eq!(format_ttl_seconds(2 * 3_600 + 30 * 60), "2h 30m");
    }
}
