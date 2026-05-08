//! S2 — Stamps screen (`docs/PLAN.md` § 8.S2).
//!
//! Renders one row per known postage batch with the volume +
//! duration framing operators actually reason about (bee#4992 is
//! retiring depth+amount). The "worst bucket" column tells the
//! truth that the API's `utilization` field is `MaxBucketCount`
//! — operators see exactly which batch is about to fail uploads
//! even though average usage is far from 100%.
//!
//! `Enter` on a selected row drills into the per-bucket histogram
//! (`/stamps/{id}/buckets`): the batch ID alone tells you the
//! worst bucket, but the drill answers the next operator question
//! — *how concentrated* is the load? A batch where 98 buckets are
//! at 90% behaves very differently from one where two are at 100%
//! and the rest are near-empty.
//!
//! Behaviour is data-driven via [`Stamps::rows_for`] and
//! [`Stamps::compute_drill_view`] so insta snapshot tests can stub
//! the input and verify status / value / `why` strings without
//! launching a TUI.

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
use crate::watch::StampsSnapshot;

use bee::postage::{BatchBucket, PostageBatch, PostageBatchBuckets};
use bee::swarm::BatchId;

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
    /// Raw seconds until expiry. Drives the TTL-threshold colour on
    /// the row even when the formatted string already coalesces to
    /// "-" / "expired".
    pub ttl_seconds: i64,
    /// `true` if `immutable` — flagged in the `value` line because
    /// mutable + full silently overwrites prior chunks (bee#5334).
    pub immutable: bool,
    pub status: StampStatus,
    /// Inline tooltip rendered on the continuation line.
    pub why: Option<String>,
}

/// TTL-threshold constants. Below `TOPUP_SOON_SECS` we suggest
/// topup in the row's `why` line; below `TOPUP_URGENT_SECS` we
/// escalate to a critical / red row. Values chosen so a healthy
/// batch never trips them: 7 days is well above the typical chain-
/// confirmation lag, and 24 h is the operator's last reaction window
/// before the batch expires.
pub const TOPUP_SOON_SECS: i64 = 7 * 24 * 3600;
pub const TOPUP_URGENT_SECS: i64 = 24 * 3600;

/// Bucket fill distribution for [`StampDrillView`]. Six buckets keep
/// the display compact while still distinguishing "nearly full" from
/// "actually full". Ordered low → high.
pub const FILL_BIN_LABELS: &[&str] = &[
    "0 %",
    "1 – 19 %",
    "20 – 49 %",
    "50 – 79 %",
    "80 – 99 %",
    "100 %",
];

/// Aggregated drill view for the bucket histogram screen. Pure —
/// computed from [`PostageBatchBuckets`] without any I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampDrillView {
    pub depth: u8,
    pub bucket_depth: u8,
    pub upper_bound: u32,
    /// Sum of every bucket's collisions count (the chunks-stamped
    /// total Bee tracks for the batch).
    pub total_chunks: u64,
    /// `2^bucket_depth × upper_bound` — the headline "what the batch
    /// could hold if perfectly distributed" number. Computed in
    /// `u128` to dodge overflow on max-depth batches.
    pub theoretical_capacity: u128,
    /// Count of buckets whose fill percentage falls in each
    /// [`FILL_BIN_LABELS`] bin. `[u32; 6]` matches the bin labels
    /// 1-for-1.
    pub fill_distribution: [u32; 6],
    /// Up to 10 worst buckets sorted by collisions descending.
    /// Stable ordering: ties broken by bucket-id ascending.
    pub worst_buckets: Vec<WorstBucket>,
    /// Worst single bucket fill percentage (matches the row's
    /// `worst_bucket_pct`).
    pub worst_pct: u32,
    /// Predicted economics — populated from the batch's `amount` and
    /// `depth` per the canonical `bzz = amount × 2^depth / 1e16`
    /// formula used by swarm-cli (`stamp/buy.ts:97-103`),
    /// beekeeper-stamper (`pkg/stamper/node.go:33-43`), and
    /// gateway-proxy (`stamps.ts:198-234`). `None` when Bee didn't
    /// supply `amount` (very rare; usually for not-yet-confirmed
    /// batches).
    pub economics: Option<StampEconomics>,
}

/// Predictive economics computed for a batch, all rendered as
/// formatted strings so the renderer doesn't hold BigInt / f64.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StampEconomics {
    /// Total BZZ pre-paid for the batch — `amount × 2^depth / 1e16`.
    /// Formatted with 4 decimal places.
    pub bzz_paid: String,
    /// Effective storage in bytes (theoretical, before bucket
    /// skew). Same value as `theoretical_capacity` × 4096; included
    /// here as a humanised string ("16.0 GiB") for the drill header.
    pub volume_humanised: String,
    /// Cost per GiB of theoretical capacity, in BZZ. Useful for
    /// comparing batches with different depths at a glance.
    pub bzz_per_gib: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorstBucket {
    pub bucket_id: u32,
    pub collisions: u32,
    pub pct: u32,
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
    /// Visual-line scroll offset for the table body. Updated lazily
    /// inside `draw_table`. Continuations (the `why` tooltip lines)
    /// count as additional visual lines so the offset is in lines,
    /// not rows.
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

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        // If batches disappear/shrink, clamp the selection so we don't
        // dangle an out-of-bounds index.
        let n = self.snapshot.batches.len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    /// Drain any drill fetches that completed since the last tick.
    /// Late results from a since-cancelled drill (operator hit Esc
    /// then Enter on a different row before the network came back)
    /// are dropped silently — `drill` already moved on.
    fn drain_fetches(&mut self) {
        while let Ok((batch_id, result)) = self.fetch_rx.try_recv() {
            match &self.drill {
                DrillState::Loading { batch_id: pending } if *pending == batch_id => {}
                _ => continue, // user moved on; ignore
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
                        view: Self::compute_drill_view(&buckets, batch),
                    }
                }
                Err(error) => DrillState::Failed { batch_id, error },
            };
        }
    }

    /// Pure, snapshot-driven row computation. Exposed for snapshot
    /// tests.
    pub fn rows_for(snap: &StampsSnapshot) -> Vec<StampRow> {
        snap.batches.iter().map(row_from_batch).collect()
    }

    /// Pure compute path for the drill pane. Buckets the per-bucket
    /// collisions into [`FILL_BIN_LABELS`] bins, picks the top-10
    /// worst, totals the chunk count. `batch` is optional — when
    /// supplied, predicted economics (`bzz_paid` etc.) are derived
    /// from `batch.amount` × `2^depth` and rendered in the drill
    /// header. Tests that only care about bucket shape can pass
    /// `None`.
    pub fn compute_drill_view(
        buckets: &PostageBatchBuckets,
        batch: Option<&PostageBatch>,
    ) -> StampDrillView {
        let upper_bound = buckets.bucket_upper_bound.max(1);
        let mut fill_distribution = [0u32; 6];
        let mut total_chunks: u64 = 0;
        for b in &buckets.buckets {
            total_chunks += u64::from(b.collisions);
            let bin = bucket_fill_bin(b.collisions, upper_bound);
            fill_distribution[bin] += 1;
        }
        let mut sorted: Vec<&BatchBucket> = buckets.buckets.iter().collect();
        // Worst first; ties broken by bucket-id ascending so the list
        // is deterministic regardless of how Bee returns the array.
        sorted.sort_by(|a, b| {
            b.collisions
                .cmp(&a.collisions)
                .then_with(|| a.bucket_id.cmp(&b.bucket_id))
        });
        let worst_buckets: Vec<WorstBucket> = sorted
            .iter()
            .take(10)
            .map(|b| WorstBucket {
                bucket_id: b.bucket_id,
                collisions: b.collisions,
                pct: pct_of(b.collisions, upper_bound),
            })
            .collect();
        let worst_pct = worst_buckets.first().map(|w| w.pct).unwrap_or(0);
        let theoretical_capacity = (1u128 << buckets.bucket_depth) * u128::from(upper_bound);
        let economics = batch.and_then(compute_stamp_economics);
        StampDrillView {
            depth: buckets.depth,
            bucket_depth: buckets.bucket_depth,
            upper_bound,
            total_chunks,
            theoretical_capacity,
            fill_distribution,
            worst_buckets,
            worst_pct,
            economics,
        }
    }

    /// Spawn a background fetch for the batch under the cursor.
    /// No-op if there are no batches or a fetch is already in
    /// flight for the same batch.
    fn maybe_start_drill(&mut self) {
        if self.snapshot.batches.is_empty() {
            return;
        }
        let i = self.selected.min(self.snapshot.batches.len() - 1);
        let batch_id = self.snapshot.batches[i].batch_id;
        if let DrillState::Loading { batch_id: pending } = &self.drill {
            if *pending == batch_id {
                return; // already in flight
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

fn bucket_fill_bin(collisions: u32, upper_bound: u32) -> usize {
    if collisions == 0 {
        return 0;
    }
    if collisions >= upper_bound {
        return 5; // 100 % (and over-saturated edge — Bee can over-report)
    }
    let pct = pct_of(collisions, upper_bound);
    match pct {
        0 => 0, // belt-and-braces; the early return handles 0
        1..=19 => 1,
        20..=49 => 2,
        50..=79 => 3,
        80..=99 => 4,
        _ => 5,
    }
}

fn pct_of(collisions: u32, upper_bound: u32) -> u32 {
    if upper_bound == 0 {
        return 0;
    }
    let pct = (u64::from(collisions) * 100) / u64::from(upper_bound);
    pct.min(100) as u32
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
    } else if b.batch_ttl <= TOPUP_URGENT_SECS {
        // Bucket headroom OK, but TTL is dangerously low. This
        // arm comes before the worst-bucket-skewed check because
        // a near-expiry batch with a fine bucket is still going
        // to fail — the bucket prediction is moot.
        (
            StampStatus::Critical,
            Some(format!(
                "topup URGENT — TTL {} (under {}h threshold).",
                ttl,
                TOPUP_URGENT_SECS / 3600
            )),
        )
    } else if worst_bucket_pct >= 80 {
        (
            StampStatus::Skewed,
            Some(format!(
                "worst bucket {worst_bucket_pct}% > safe headroom — dilute or stop using."
            )),
        )
    } else if b.batch_ttl <= TOPUP_SOON_SECS {
        // Healthy buckets but TTL low enough to plan ahead. Don't
        // escalate to Skewed (operator might mis-read it as bucket
        // skew); use a milder Skewed-for-time framing.
        (
            StampStatus::Skewed,
            Some(format!(
                "topup soon — TTL {} (under {}d planning threshold).",
                ttl,
                TOPUP_SOON_SECS / 86_400
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
        ttl_seconds: b.batch_ttl,
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

/// Predicted economics for a batch. Replicates the canonical
/// formula used across the ecosystem: `bzz_paid = amount × 2^depth
/// / 1e16` (swarm-cli `stamp/buy.ts:97-103`, beekeeper-stamper
/// `pkg/stamper/node.go:33-43`, gateway-proxy `stamps.ts:198-234`).
///
/// Returns `None` when Bee did not supply `amount` (rare; usually
/// for not-yet-confirmed batches). f64 conversion is precise enough:
/// realistic batches yield ≤ 10³ BZZ which leaves > 12 decimal
/// digits of headroom in f64.
fn compute_stamp_economics(b: &PostageBatch) -> Option<StampEconomics> {
    let amount = b.amount.as_ref()?;
    let two_pow_depth: num_bigint::BigInt = num_bigint::BigInt::from(1u32) << b.depth as usize;
    let total_plur = amount * &two_pow_depth;
    let bzz: f64 = total_plur.to_string().parse::<f64>().ok()? / 1e16;

    let cap_bytes: u128 = (1u128 << b.depth) * 4096;
    let volume_humanised = format_bytes(cap_bytes);

    const GIB: f64 = 1024.0 * 1024.0 * 1024.0;
    let gib = cap_bytes as f64 / GIB;
    let bzz_per_gib = if gib > 0.0 {
        format!("{:.4} BZZ/GiB", bzz / gib)
    } else {
        "n/a".to_string()
    };

    Some(StampEconomics {
        bzz_paid: format!("{bzz:.4} BZZ"),
        volume_humanised,
        bzz_per_gib,
    })
}

/// Bytes → IEC binary (KiB / MiB / GiB / TiB).
pub(crate) fn format_bytes(bytes: u128) -> String {
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

pub(crate) fn format_ttl_seconds(secs: i64) -> String {
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
            self.drain_fetches();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        // Drill mode swallows Esc to dismiss; otherwise keys behave
        // the same as in list mode (so `j`/`k` etc. don't surprise
        // the operator after pressing Enter).
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
            Constraint::Length(3), // header
            Constraint::Min(0),    // body (table or drill)
            Constraint::Length(1), // selected detail (full IDs)
            Constraint::Length(1), // footer
        ])
        .split(area);

        // Header
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
            // Render the full hex so operators can click-drag to copy
            // for block-explorer / Discord / support-thread use. The
            // batch ID is 64 chars — fits cleanly on the second line.
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

        // Body
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

        // Selected detail — full batch ID + label of the highlighted
        // row, rendered as a separate line so operators can click-
        // drag to copy without entering drill mode. Only meaningful
        // in table view; suppressed during drill since the drill
        // header already prints the full ID.
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

        // Footer — keymap shifts in drill mode.
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
        use ratatui::layout::{Constraint, Layout};

        let t = theme::active();

        // Pinned column header + scrollable body, same pattern as
        // S6. Header doesn't scroll out from under the cursor.
        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "   LABEL                BATCH        VOLUME      WORST BUCKET                TTL         STATUS",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::BOLD),
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
        // Map batch index → visual line index where that row's main
        // line starts. Used by `clamp_scroll` to translate a row
        // selection into a visual-line offset, since rows may emit
        // 1 or 2 lines depending on whether they have a `why`
        // continuation.
        let mut row_starts: Vec<usize> = Vec::new();
        for (i, r) in Self::rows_for(&self.snapshot).into_iter().enumerate() {
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
                        .fg(r.status.color())
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

        // Translate the row cursor → visual-line cursor. Use the
        // first line of the selected row as the "selected visual
        // line" so clamp_scroll keeps that row's main line on
        // screen (the continuation tooltip will follow if it fits).
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
        // Headline summary line.
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
            // Predictive economics line — same `amount × 2^depth /
            // 1e16` formula as swarm-cli/beekeeper-stamper. Lets
            // operators sanity-check what they paid for this batch
            // alongside the bucket telemetry.
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

        // Fill-distribution histogram.
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
            // Bin colour follows the fill range so the operator's eye
            // jumps to the rows that matter (red 100 %, yellow 80–99,
            // pass otherwise).
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

        // Top-N worst buckets.
        if !view.worst_buckets.is_empty() {
            lines.push(Line::from(Span::styled(
                "  WORST BUCKETS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            )));
            for w in &view.worst_buckets {
                if w.collisions == 0 {
                    // Once we hit zero-collision buckets the rest are
                    // also zero — don't pad the worst-N with junk.
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

    fn buckets_with(counts: &[(u32, u32)], depth: u8, bucket_depth: u8) -> PostageBatchBuckets {
        let upper_bound = 1u32 << (depth - bucket_depth);
        let buckets = counts
            .iter()
            .map(|(id, c)| BatchBucket {
                bucket_id: *id,
                collisions: *c,
            })
            .collect();
        PostageBatchBuckets {
            depth,
            bucket_depth,
            bucket_upper_bound: upper_bound,
            buckets,
        }
    }

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

    #[test]
    fn drill_view_bins_and_worst_n() {
        // depth=22, bucket_depth=16 → upper_bound=64. Construct a
        // small sample with one full bucket, one near-full, two
        // mid, rest empty.
        let buckets = buckets_with(
            &[
                (0, 64), // 100 %  → bin 5
                (1, 60), // 93 %   → bin 4
                (2, 40), // 62 %   → bin 3
                (3, 20), // 31 %   → bin 2
                (4, 1),  // 1 %    → bin 1
                (5, 0),  // 0 %    → bin 0
            ],
            22,
            16,
        );
        let view = Stamps::compute_drill_view(&buckets, None);
        assert_eq!(view.depth, 22);
        assert_eq!(view.bucket_depth, 16);
        assert_eq!(view.upper_bound, 64);
        assert_eq!(view.total_chunks, 64 + 60 + 40 + 20 + 1);
        assert_eq!(view.fill_distribution, [1, 1, 1, 1, 1, 1]);
        assert_eq!(view.worst_pct, 100);
        // Worst-N is the worst-3 here (rest are zero — truncated).
        // The order is: bucket 0 (64), bucket 1 (60), bucket 2 (40),
        // then 20, 1; the trailing zero-bucket is included only up
        // to 10 entries — we filter zero-collisions in the renderer
        // but compute_drill_view includes them up to the cap.
        assert_eq!(view.worst_buckets.len(), 6);
        assert_eq!(view.worst_buckets[0].bucket_id, 0);
        assert_eq!(view.worst_buckets[0].pct, 100);
        assert_eq!(view.worst_buckets[1].bucket_id, 1);
        assert_eq!(view.worst_buckets[1].pct, 93);
    }

    #[test]
    fn drill_view_handles_empty_buckets() {
        let buckets = buckets_with(&[], 22, 16);
        let view = Stamps::compute_drill_view(&buckets, None);
        assert_eq!(view.total_chunks, 0);
        assert_eq!(view.fill_distribution, [0; 6]);
        assert_eq!(view.worst_pct, 0);
        assert!(view.worst_buckets.is_empty());
    }

    #[test]
    fn drill_view_caps_worst_at_ten() {
        // 12 distinct buckets, all collisions=1 — top-10 should
        // truncate at 10 entries.
        let entries: Vec<(u32, u32)> = (0..12).map(|i| (i, 1)).collect();
        let buckets = buckets_with(&entries, 22, 16);
        let view = Stamps::compute_drill_view(&buckets, None);
        assert_eq!(view.worst_buckets.len(), 10);
    }

    #[test]
    fn drill_view_breaks_ties_by_bucket_id() {
        let buckets = buckets_with(&[(7, 5), (3, 5), (10, 5)], 22, 16);
        let view = Stamps::compute_drill_view(&buckets, None);
        // All three tie on collisions=5 → ascending by bucket_id.
        assert_eq!(
            view.worst_buckets
                .iter()
                .map(|w| w.bucket_id)
                .collect::<Vec<_>>(),
            vec![3, 7, 10],
        );
    }

    #[test]
    fn fill_bin_handles_overflow_collisions() {
        // Bee occasionally over-reports collisions vs upper_bound.
        // Treat it as the saturated 100 % bin rather than panicking.
        assert_eq!(bucket_fill_bin(70, 64), 5);
    }

    fn make_batch(amount: Option<num_bigint::BigInt>, depth: u8) -> PostageBatch {
        PostageBatch {
            batch_id: bee::swarm::BatchId::new(&[0u8; 32]).unwrap(),
            amount,
            start: 0,
            owner: String::new(),
            depth,
            bucket_depth: depth.saturating_sub(6),
            immutable: true,
            batch_ttl: 30 * 86_400,
            utilization: 0,
            usable: true,
            exists: true,
            label: "test".into(),
            block_number: 0,
        }
    }

    #[test]
    fn economics_returns_none_when_amount_missing() {
        let b = make_batch(None, 22);
        assert!(compute_stamp_economics(&b).is_none());
    }

    #[test]
    fn economics_typical_batch_formats_strings() {
        // amount=1e14 PLUR/chunk, depth=22 → 2^22 = 4_194_304 chunks
        // total = 1e14 × 4_194_304 = 4.194304e20 PLUR
        // bzz_paid = 4.194304e20 / 1e16 = 41943.04 BZZ
        // capacity = 4_194_304 × 4096 = 16 GiB exactly
        // bzz_per_gib = 41943.04 / 16 = 2621.44 BZZ/GiB
        let amount = num_bigint::BigInt::from(100_000_000_000_000u64);
        let b = make_batch(Some(amount), 22);
        let e = compute_stamp_economics(&b).expect("amount present");
        assert_eq!(e.bzz_paid, "41943.0400 BZZ");
        assert_eq!(e.volume_humanised, "16.0 GiB");
        assert_eq!(e.bzz_per_gib, "2621.4400 BZZ/GiB");
    }

    #[test]
    fn economics_wired_through_compute_drill_view() {
        let amount = num_bigint::BigInt::from(100_000_000_000_000u64);
        let batch = make_batch(Some(amount), 22);
        let buckets = buckets_with(&[(0, 1)], 22, 16);
        let view = Stamps::compute_drill_view(&buckets, Some(&batch));
        let e = view.economics.as_ref().expect("economics populated");
        assert_eq!(e.volume_humanised, "16.0 GiB");
    }

    #[test]
    fn row_topup_urgent_when_ttl_under_24h_with_healthy_buckets() {
        // Bucket headroom is fine but TTL has dropped below 24h —
        // the bucket-skewed arm shouldn't fire; the urgent arm wins.
        let mut b = make_batch(None, 22);
        b.batch_ttl = 12 * 3_600;
        b.utilization = 14; // ~22 %, healthy
        let row = row_from_batch(&b);
        assert_eq!(row.status, StampStatus::Critical);
        assert!(row.why.as_ref().unwrap().contains("topup URGENT"));
    }

    #[test]
    fn row_topup_soon_when_ttl_under_7d_with_healthy_buckets() {
        // 3 days left, buckets fine. Should be Skewed with the
        // planning-threshold tooltip.
        let mut b = make_batch(None, 22);
        b.batch_ttl = 3 * 86_400;
        b.utilization = 14;
        let row = row_from_batch(&b);
        assert_eq!(row.status, StampStatus::Skewed);
        assert!(row.why.as_ref().unwrap().contains("topup soon"));
    }

    #[test]
    fn row_critical_bucket_wins_over_urgent_ttl() {
        // Worst bucket already at 95 %+ AND TTL is under 24h. The
        // bucket-driven critical message takes precedence (operators
        // usually act on bucket fill before TTL anyway).
        let mut b = make_batch(None, 22);
        b.batch_ttl = 12 * 3_600;
        b.utilization = 63; // 98 %
        let row = row_from_batch(&b);
        assert_eq!(row.status, StampStatus::Critical);
        let why = row.why.as_ref().unwrap();
        assert!(!why.contains("topup URGENT"));
        assert!(why.contains("REJECT") || why.contains("overwrite"));
    }
}
