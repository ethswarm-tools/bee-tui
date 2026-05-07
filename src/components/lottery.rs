//! S4 — Lottery / redistribution screen (`docs/PLAN.md` § 8.S4).
//!
//! Three panes driven by [`crate::watch::HealthSnapshot`] (for
//! `/redistributionstate` at 2 s cadence) and
//! [`crate::watch::LotterySnapshot`] (for `/stake` at 30 s):
//!
//! 1. **Round timeline** — current round number plus a three-segment
//!    bar for the commit (0..38) / reveal (38..76) / claim (76..152)
//!    phases, with the active phase highlighted and a within-phase
//!    progress bar. Block-of-round comes from `block % 152` per the
//!    upstream `DefaultBlocksPerRound` constant in
//!    `bee-go/pkg/storageincentives/agent.go`.
//! 2. **Anchor summary** — last *won*, *played*, *frozen*, *selected*
//!    rounds, each with a `Δ` to the current round so operators see
//!    "last won 4 rounds ago" without doing arithmetic. The PLAN's
//!    "last 20 rounds with skip reasons" view needs an upstream
//!    `RoundData[]` port and is deferred.
//! 3. **Stake card** — staked BZZ vs the minimum needed to play, with
//!    the "frozen / unhealthy / insufficient funds" reasons that
//!    Bee's API exposes only in scattered booleans surfaced as a
//!    single human sentence.
//!
//! Render path delegates to the pure [`Lottery::view_for`] function so
//! `tests/s4_lottery_view.rs` can pin all state derivations
//! (phase math, skip-reason reconstruction, stake-status ladder)
//! without a TUI.

use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use num_bigint::BigInt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::{mpsc, watch};

use super::Component;
use super::swap::format_plur;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::{HealthSnapshot, LotterySnapshot};

use bee::debug::{RCHashResponse, RedistributionState};

/// `pkg/storageincentives/agent.go:36` — round length in blocks.
pub const BLOCKS_PER_ROUND: u64 = 152;
/// One-quarter of the round (152 / 4); commit and reveal each take this many
/// blocks, claim takes the remaining 76.
pub const BLOCKS_PER_PHASE: u64 = 38;

/// Discrete phase enum derived from the API's `phase: String`. Falls
/// back to [`Phase::Unknown`] if Bee returns a value we don't model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Phase {
    Commit,
    Reveal,
    Claim,
    /// In-between rounds — the agent is sampling chunks for the next
    /// commit. Bee surfaces this as `phase: "sample"`.
    Sample,
    Unknown,
}

impl Phase {
    fn from_api(s: &str) -> Self {
        match s {
            "commit" => Self::Commit,
            "reveal" => Self::Reveal,
            "claim" => Self::Claim,
            "sample" => Self::Sample,
            _ => Self::Unknown,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Commit => "commit",
            Self::Reveal => "reveal",
            Self::Claim => "claim",
            Self::Sample => "sample",
            Self::Unknown => "?",
        }
    }
}

/// Tri-state per-phase outcome shown in the timeline ribbon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PhaseState {
    /// Phase already completed in the current round.
    Done,
    /// Currently in this phase.
    Active,
    /// Phase hasn't started yet.
    Pending,
}

/// One segment of the timeline ribbon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PhaseSegment {
    pub phase: Phase,
    pub state: PhaseState,
    /// Inclusive lower bound of this phase in block-of-round.
    pub start_block: u64,
    /// Exclusive upper bound.
    pub end_block: u64,
}

/// Top pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RoundCard {
    pub round: u64,
    pub block: u64,
    /// `block % BLOCKS_PER_ROUND` for quick visual reference.
    pub block_of_round: u64,
    /// Current phase as enum + label.
    pub phase: Phase,
    pub phase_label: &'static str,
    pub segments: Vec<PhaseSegment>,
}

/// One row of the anchor-summary pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchorRow {
    pub label: &'static str,
    pub round: u64,
    /// `current_round - round`. `None` if the anchor round is 0
    /// (never reached) or in the future.
    pub delta: Option<u64>,
    /// Human description: `"never"`, `"4 rounds ago"`, etc.
    pub when: String,
}

/// Tri-state stake card status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StakeStatus {
    /// `staked == 0` — node can't participate.
    Unstaked,
    /// 0 < staked but `has_sufficient_funds == false` — operator funded
    /// the contract but the wallet doesn't have gas, so the node skips.
    InsufficientGas,
    /// Frozen out of redistribution by a previous bad commit.
    Frozen,
    /// Worker is unhealthy (storage radius drift, sample failures, etc.).
    Unhealthy,
    /// `staked > 0`, `has_sufficient_funds`, not frozen, healthy.
    Healthy,
    /// `/stake` failed.
    Unknown,
}

impl StakeStatus {
    fn color(self) -> Color {
        match self {
            Self::Unstaked => theme::active().fail,
            Self::InsufficientGas => theme::active().warn,
            Self::Frozen => theme::active().fail,
            Self::Unhealthy => theme::active().warn,
            Self::Healthy => theme::active().pass,
            Self::Unknown => theme::active().dim,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Unstaked => "✗ unstaked",
            Self::InsufficientGas => "⚠ low gas",
            Self::Frozen => "✗ frozen",
            Self::Unhealthy => "⚠ unhealthy",
            Self::Healthy => "✓ healthy",
            Self::Unknown => "? unknown",
        }
    }
}

/// Bottom pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StakeCard {
    pub status: StakeStatus,
    /// Pre-formatted `BZZ x.xxxx` or `—`.
    pub staked: String,
    pub minimum_gas: String,
    /// Cumulative reward (`BZZ x.xxxx` or `—`).
    pub reward: String,
    pub fees: String,
    /// Last sample duration in seconds, formatted to one decimal.
    pub last_sample: Option<String>,
    /// Tooltip explaining the current state when not Healthy.
    pub why: Option<String>,
}

/// Aggregated view fed to both renderer and snapshot tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LotteryView {
    pub round: Option<RoundCard>,
    pub anchors: Vec<AnchorRow>,
    pub stake: StakeCard,
}

/// Lifecycle of the on-demand rchash benchmark. Operator hits `r` to
/// run; the component owns the request lifecycle via an internal mpsc
/// channel so the App's action pipeline doesn't grow a one-off
/// rchash-result variant.
#[derive(Debug, Clone, PartialEq)]
pub enum BenchState {
    Idle,
    Running,
    Done {
        duration_seconds: f64,
        /// Reserve commitment hash (8-char prefix is what the screen
        /// shows; full hash is kept here for the sake of completeness).
        hash: String,
    },
    Failed {
        error: String,
    },
}

/// Fixed anchor pair used for the benchmark. Real-round anchors come
/// off-chain; for a deterministic local sample we use 0x00…00 vs
/// 0xff…ff so repeat measurements compare cleanly.
const BENCH_ANCHOR_LO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const BENCH_ANCHOR_HI: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";
/// Fallback depth when `/status` hasn't reported a `storage_radius`
/// yet. 8 is a typical mainnet radius, so the sample size is
/// representative.
const BENCH_DEFAULT_DEPTH: u8 = 8;

/// Pick a depth for the benchmark that mirrors what the agent would
/// actually sample at: the node's current `storage_radius`. Falls back
/// to [`BENCH_DEFAULT_DEPTH`] if `/status` hasn't loaded yet. Status's
/// `storage_radius` is `i64` because the API can return a sentinel
/// `-1`; we clamp to `0..=255` for the rchash path.
pub fn bench_depth(health: &HealthSnapshot) -> u8 {
    let raw = health
        .status
        .as_ref()
        .map(|s| s.storage_radius)
        .unwrap_or(-1);
    if raw <= 0 {
        BENCH_DEFAULT_DEPTH
    } else {
        raw.min(255) as u8
    }
}

pub struct Lottery {
    client: Arc<ApiClient>,
    health_rx: watch::Receiver<HealthSnapshot>,
    lottery_rx: watch::Receiver<LotterySnapshot>,
    health: HealthSnapshot,
    lottery: LotterySnapshot,
    bench: BenchState,
    bench_tx: mpsc::UnboundedSender<Result<RCHashResponse, String>>,
    bench_rx: mpsc::UnboundedReceiver<Result<RCHashResponse, String>>,
}

impl Lottery {
    pub fn new(
        client: Arc<ApiClient>,
        health_rx: watch::Receiver<HealthSnapshot>,
        lottery_rx: watch::Receiver<LotterySnapshot>,
    ) -> Self {
        let health = health_rx.borrow().clone();
        let lottery = lottery_rx.borrow().clone();
        let (bench_tx, bench_rx) = mpsc::unbounded_channel();
        Self {
            client,
            health_rx,
            lottery_rx,
            health,
            lottery,
            bench: BenchState::Idle,
            bench_tx,
            bench_rx,
        }
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.lottery = self.lottery_rx.borrow().clone();
    }

    /// Drain any benchmark results that arrived since the last tick.
    fn drain_bench_results(&mut self) {
        while let Ok(result) = self.bench_rx.try_recv() {
            self.bench = match result {
                Ok(resp) => BenchState::Done {
                    duration_seconds: resp.duration_seconds,
                    hash: resp.hash,
                },
                Err(e) => BenchState::Failed { error: e },
            };
        }
    }

    /// Kick off a benchmark unless one is already in flight. Returns
    /// `true` if a new request was spawned.
    fn maybe_start_bench(&mut self) -> bool {
        if matches!(self.bench, BenchState::Running) {
            return false;
        }
        let depth = bench_depth(&self.health);
        let client = self.client.clone();
        let tx = self.bench_tx.clone();
        tokio::spawn(async move {
            let res = client
                .bee()
                .debug()
                .r_chash(depth, BENCH_ANCHOR_LO, BENCH_ANCHOR_HI)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        self.bench = BenchState::Running;
        true
    }

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests in `tests/s4_lottery_view.rs`.
    pub fn view_for(health: &HealthSnapshot, lottery: &LotterySnapshot) -> LotteryView {
        let round = health.redistribution.as_ref().map(round_card_for);
        let anchors = health
            .redistribution
            .as_ref()
            .map(anchor_rows_for)
            .unwrap_or_default();
        let stake = stake_card_for(health.redistribution.as_ref(), lottery);
        LotteryView {
            round,
            anchors,
            stake,
        }
    }
}

fn round_card_for(r: &RedistributionState) -> RoundCard {
    let block_of_round = r.block % BLOCKS_PER_ROUND;
    let phase = Phase::from_api(&r.phase);

    // Build the three on-chain phase segments. (Sample is between rounds and
    // doesn't get a segment — when phase==Sample, the ribbon shows all three
    // segments as Done.)
    let segments = build_phase_segments(phase, block_of_round);

    RoundCard {
        round: r.round,
        block: r.block,
        block_of_round,
        phase,
        phase_label: phase.label(),
        segments,
    }
}

fn build_phase_segments(current: Phase, block_of_round: u64) -> Vec<PhaseSegment> {
    let phases = [
        (Phase::Commit, 0u64, BLOCKS_PER_PHASE),
        (Phase::Reveal, BLOCKS_PER_PHASE, 2 * BLOCKS_PER_PHASE),
        (Phase::Claim, 2 * BLOCKS_PER_PHASE, BLOCKS_PER_ROUND),
    ];
    phases
        .iter()
        .map(|&(p, start, end)| PhaseSegment {
            phase: p,
            state: phase_state_for(p, current, block_of_round, start, end),
            start_block: start,
            end_block: end,
        })
        .collect()
}

fn phase_state_for(
    seg: Phase,
    current: Phase,
    block_of_round: u64,
    start: u64,
    end: u64,
) -> PhaseState {
    // Trust the API's `phase` field for the active marker, but fall back to
    // block-range arithmetic when it reports `sample` (between rounds —
    // every commit/reveal/claim is Done) or `unknown`.
    if current == seg {
        return PhaseState::Active;
    }
    if matches!(current, Phase::Sample) {
        return PhaseState::Done;
    }
    if block_of_round >= end {
        PhaseState::Done
    } else if block_of_round < start {
        PhaseState::Pending
    } else {
        // Block-of-round is inside our segment but the API claims a different
        // phase — surface as Active anyway, the API is authoritative for the
        // current marker.
        PhaseState::Active
    }
}

fn anchor_rows_for(r: &RedistributionState) -> Vec<AnchorRow> {
    let current = r.round;
    let make = |label: &'static str, anchor: u64| AnchorRow {
        label,
        round: anchor,
        delta: if anchor == 0 || anchor > current {
            None
        } else {
            Some(current - anchor)
        },
        when: format_when(current, anchor),
    };
    vec![
        make("last won", r.last_won_round),
        make("last played", r.last_played_round),
        make("last selected", r.last_selected_round),
        make("last frozen", r.last_frozen_round),
    ]
}

fn format_when(current: u64, anchor: u64) -> String {
    if anchor == 0 {
        return "never".into();
    }
    if anchor > current {
        return format!("round {anchor} (future)");
    }
    let delta = current - anchor;
    match delta {
        0 => "this round".into(),
        1 => "last round".into(),
        n => format!("{n} rounds ago"),
    }
}

fn stake_card_for(r: Option<&RedistributionState>, lottery: &LotterySnapshot) -> StakeCard {
    let zero = BigInt::from(0);
    let staked_bi = lottery.staked.as_ref();
    let staked_str = staked_bi.map(format_plur).unwrap_or_else(|| "—".into());

    let (minimum_gas, reward, fees, last_sample, status_inputs) = match r {
        Some(r) => (
            r.minimum_gas_funds
                .as_ref()
                .map(format_plur)
                .unwrap_or_else(|| "—".into()),
            r.reward
                .as_ref()
                .map(format_plur)
                .unwrap_or_else(|| "—".into()),
            r.fees
                .as_ref()
                .map(format_plur)
                .unwrap_or_else(|| "—".into()),
            (r.last_sample_duration_seconds > 0.0)
                .then(|| format!("{:.1}s", r.last_sample_duration_seconds)),
            Some((
                r.is_frozen,
                r.is_healthy,
                r.has_sufficient_funds,
                r.is_fully_synced,
                r.last_frozen_round,
            )),
        ),
        None => ("—".into(), "—".into(), "—".into(), None, None),
    };

    let (status, why) = match (lottery.last_error.as_deref(), staked_bi, status_inputs) {
        (Some(e), _, _) => (StakeStatus::Unknown, Some(format!("/stake error: {e}"))),
        (_, None, _) => (StakeStatus::Unknown, Some("/stake not loaded yet".into())),
        (_, Some(s), Some((frozen, healthy, sufficient, synced, last_frozen))) if s == &zero => {
            let _ = (frozen, healthy, sufficient, synced, last_frozen);
            (
                StakeStatus::Unstaked,
                Some("0 BZZ staked — node cannot participate in redistribution.".into()),
            )
        }
        (_, Some(_), Some((true, _, _, _, last_frozen))) => (
            StakeStatus::Frozen,
            Some(format!(
                "frozen out at round {last_frozen}; resumes after the freeze window."
            )),
        ),
        (_, Some(_), Some((_, _, false, _, _))) => (
            StakeStatus::InsufficientGas,
            Some("operator wallet has too little native token to play a round.".into()),
        ),
        (_, Some(_), Some((_, false, _, _, _))) => (
            StakeStatus::Unhealthy,
            Some("redistribution worker reports unhealthy — see Health screen.".into()),
        ),
        (_, Some(_), Some((_, _, _, false, _))) => (
            StakeStatus::Unhealthy,
            Some("node is not fully synced — sampling will skip until it catches up.".into()),
        ),
        (_, Some(_), Some(_)) => (StakeStatus::Healthy, None),
        // Stake known but no redistribution data yet.
        (_, Some(s), None) if s == &zero => (
            StakeStatus::Unstaked,
            Some("0 BZZ staked — node cannot participate in redistribution.".into()),
        ),
        (_, Some(_), None) => (
            StakeStatus::Unknown,
            Some("redistribution state not loaded yet".into()),
        ),
    };

    StakeCard {
        status,
        staked: staked_str,
        minimum_gas,
        reward,
        fees,
        last_sample,
        why,
    }
}

impl Component for Lottery {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
            self.drain_bench_results();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if matches!(key.code, KeyCode::Char('r')) {
            self.maybe_start_bench();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Length(6), // round timeline
            Constraint::Length(7), // anchors
            Constraint::Min(0),    // stake card
            Constraint::Length(1), // footer
        ])
        .split(area);

        // Header
        let header_l1 = Line::from(vec![Span::styled(
            "LOTTERY / REDISTRIBUTION",
            Style::default().add_modifier(Modifier::BOLD),
        )]);
        let mut header_l2 = Vec::new();
        let t = theme::active();
        if let Some(err) = &self.lottery.last_error {
            let (color, msg) = theme::classify_header_error(err);
            header_l2.push(Span::styled(msg, Style::default().fg(color)));
        } else if !self.lottery.is_loaded() {
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

        let view = Self::view_for(&self.health, &self.lottery);

        // Round timeline
        let mut round_lines: Vec<Line> = Vec::new();
        if let Some(rc) = &view.round {
            round_lines.push(Line::from(vec![
                Span::styled(
                    format!("  Round {} ", rc.round),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "· phase {} · block-of-round {}/{BLOCKS_PER_ROUND}",
                        rc.phase_label, rc.block_of_round
                    ),
                    Style::default().fg(t.dim),
                ),
            ]));
            round_lines.push(Line::from(segment_spans(&rc.segments)));
            round_lines.push(Line::from(progress_bar_spans(rc)));
        } else {
            round_lines.push(Line::from(Span::styled(
                "  (redistribution state not loaded yet)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        frame.render_widget(
            Paragraph::new(round_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        // Anchors
        let mut anchor_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  ANCHORS         ROUND       WHEN",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if view.anchors.is_empty() {
            anchor_lines.push(Line::from(Span::styled(
                "  (no anchor data)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            for a in &view.anchors {
                let round_str = if a.round == 0 {
                    "—".to_string()
                } else {
                    a.round.to_string()
                };
                anchor_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<14} ", a.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{round_str:<11} ")),
                    Span::styled(a.when.clone(), Style::default().fg(t.dim)),
                ]));
            }
        }
        frame.render_widget(
            Paragraph::new(anchor_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[2],
        );

        // Stake card
        let stake = &view.stake;
        let mut stake_lines = vec![
            Line::from(vec![
                Span::styled("  Stake  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    stake.status.label(),
                    Style::default()
                        .fg(stake.status.color())
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw(format!("    staked     {}", stake.staked)),
                Span::raw("   "),
                Span::raw(format!("min gas funds  {}", stake.minimum_gas)),
            ]),
            Line::from(vec![
                Span::raw(format!("    reward     {}", stake.reward)),
                Span::raw("   "),
                Span::raw(format!("fees           {}", stake.fees)),
            ]),
        ];
        if let Some(sample) = &stake.last_sample {
            stake_lines.push(Line::from(vec![
                Span::raw("    last sample "),
                Span::styled(sample.clone(), Style::default().fg(t.info)),
                Span::styled(
                    "   (deadline ≈ 95s for the 38-block commit window)",
                    Style::default().fg(t.dim),
                ),
            ]));
        }
        if let Some(why) = &stake.why {
            stake_lines.push(Line::from(vec![
                Span::raw("    └─ "),
                Span::styled(
                    why.clone(),
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        // Bench card: rchash benchmark line. Always rendered so the
        // operator knows the keybinding exists.
        let depth = bench_depth(&self.health);
        stake_lines.push(Line::from(""));
        stake_lines.push(Line::from(vec![
            Span::styled(
                "  rchash bench  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("(depth {depth}, deterministic anchors)"),
                Style::default().fg(t.dim),
            ),
        ]));
        match &self.bench {
            BenchState::Idle => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("press 'r' to run a sample", Style::default().fg(t.dim)),
                ]));
            }
            BenchState::Running => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        "running… (this can take seconds-to-minutes on a busy reserve)",
                        Style::default().fg(t.info),
                    ),
                ]));
            }
            BenchState::Done {
                duration_seconds,
                hash,
            } => {
                let prefix: String = hash.chars().take(8).collect();
                let safe = *duration_seconds < 95.0;
                let style = if safe {
                    Style::default().fg(t.pass)
                } else {
                    Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                };
                let verdict = if safe {
                    "safe — fits inside the 95 s commit window"
                } else {
                    "OVER 95 s commit window — sampler will time out!"
                };
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("{duration_seconds:.1}s"), style),
                    Span::raw(format!("   hash {prefix}…   ")),
                    Span::styled(verdict, Style::default().fg(t.dim)),
                ]));
            }
            BenchState::Failed { error } => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("error: {error}"), Style::default().fg(t.fail)),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(stake_lines), chunks[3]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" r ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" run rchash benchmark  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
            ])),
            chunks[4],
        );

        Ok(())
    }
}

fn segment_spans(segs: &[PhaseSegment]) -> Vec<Span<'static>> {
    let t = theme::active();
    let mut out = vec![Span::raw("  ")];
    for (i, s) in segs.iter().enumerate() {
        let color = match s.state {
            PhaseState::Done => t.dim,
            PhaseState::Active => t.warn,
            PhaseState::Pending => Color::White,
        };
        let modifier = if matches!(s.state, PhaseState::Active) {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        out.push(Span::styled(
            format!(" {} {}..{} ", s.phase.label(), s.start_block, s.end_block),
            Style::default().fg(color).add_modifier(modifier),
        ));
        if i + 1 < segs.len() {
            out.push(Span::styled("│", Style::default().fg(t.dim)));
        }
    }
    out
}

fn progress_bar_spans(rc: &RoundCard) -> Vec<Span<'static>> {
    // 24-cell ASCII bar over 0..152.
    const WIDTH: usize = 24;
    let filled = ((rc.block_of_round as usize) * WIDTH) / BLOCKS_PER_ROUND as usize;
    let mut bar = String::with_capacity(WIDTH);
    for _ in 0..filled.min(WIDTH) {
        bar.push('▇');
    }
    for _ in filled.min(WIDTH)..WIDTH {
        bar.push('░');
    }
    vec![
        Span::raw("  "),
        Span::styled(bar, Style::default().fg(theme::active().warn)),
        Span::raw(format!("   {}/{BLOCKS_PER_ROUND}", rc.block_of_round)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_from_api_known() {
        assert_eq!(Phase::from_api("commit"), Phase::Commit);
        assert_eq!(Phase::from_api("reveal"), Phase::Reveal);
        assert_eq!(Phase::from_api("claim"), Phase::Claim);
        assert_eq!(Phase::from_api("sample"), Phase::Sample);
    }

    #[test]
    fn phase_from_api_unknown_falls_back() {
        assert_eq!(Phase::from_api(""), Phase::Unknown);
        assert_eq!(Phase::from_api("garbage"), Phase::Unknown);
    }

    #[test]
    fn phase_segments_during_commit() {
        let segs = build_phase_segments(Phase::Commit, 10);
        assert_eq!(segs[0].state, PhaseState::Active);
        assert_eq!(segs[1].state, PhaseState::Pending);
        assert_eq!(segs[2].state, PhaseState::Pending);
    }

    #[test]
    fn phase_segments_during_reveal() {
        let segs = build_phase_segments(Phase::Reveal, 50);
        assert_eq!(segs[0].state, PhaseState::Done);
        assert_eq!(segs[1].state, PhaseState::Active);
        assert_eq!(segs[2].state, PhaseState::Pending);
    }

    #[test]
    fn phase_segments_during_claim() {
        let segs = build_phase_segments(Phase::Claim, 100);
        assert_eq!(segs[0].state, PhaseState::Done);
        assert_eq!(segs[1].state, PhaseState::Done);
        assert_eq!(segs[2].state, PhaseState::Active);
    }

    #[test]
    fn phase_segments_during_sample() {
        // Sample sits between rounds — every on-chain phase reads as Done.
        let segs = build_phase_segments(Phase::Sample, 0);
        for s in &segs {
            assert_eq!(s.state, PhaseState::Done);
        }
    }

    #[test]
    fn format_when_handles_zero() {
        assert_eq!(format_when(100, 0), "never");
    }

    #[test]
    fn format_when_handles_current() {
        assert_eq!(format_when(100, 100), "this round");
    }

    #[test]
    fn format_when_handles_n_ago() {
        assert_eq!(format_when(100, 95), "5 rounds ago");
    }

    #[test]
    fn bench_depth_falls_back_when_status_missing() {
        assert_eq!(bench_depth(&HealthSnapshot::default()), BENCH_DEFAULT_DEPTH);
    }

    #[test]
    fn bench_depth_falls_back_on_sentinel() {
        // Bee returns storage_radius = -1 before warmup completes.
        let snap = HealthSnapshot {
            status: Some(bee::debug::Status {
                storage_radius: -1,
                ..bee::debug::Status::default()
            }),
            ..HealthSnapshot::default()
        };
        assert_eq!(bench_depth(&snap), BENCH_DEFAULT_DEPTH);
    }

    #[test]
    fn bench_depth_uses_storage_radius_when_present() {
        let snap = HealthSnapshot {
            status: Some(bee::debug::Status {
                storage_radius: 12,
                ..bee::debug::Status::default()
            }),
            ..HealthSnapshot::default()
        };
        assert_eq!(bench_depth(&snap), 12);
    }
}
