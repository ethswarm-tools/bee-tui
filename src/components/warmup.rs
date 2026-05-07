//! S5 — Warmup screen (`docs/PLAN.md` § 8.S5).
//!
//! Surfaces the 25–60 minute cold-start opacity (bee#4746) — the
//! interval where Bee is internally bootstrapping but the operator
//! sees nothing actionable. Renders an elapsed counter plus a
//! checklist of warmup steps driven by:
//!
//! - [`crate::watch::HealthSnapshot`] — `is_warming_up`,
//!   `connected_peers`, `reserve_size_within_radius`.
//! - [`crate::watch::StampsSnapshot`] — postage batch count for the
//!   "snapshot loaded" step.
//! - [`crate::watch::TopologySnapshot`] — kademlia depth and per-bin
//!   data for the "depth stable" and bin-relative steps.
//!
//! The component remains useful after warmup completes: every step
//! latches to Done and the elapsed counter freezes, so operators can
//! see a "definition of done" of the bootstrap process even on a
//! healthy node.
//!
//! Render delegates to the pure [`Warmup::view_for`] so the snapshot
//! tests in `tests/s5_warmup_view.rs` can pin every step's state
//! without launching a TUI or dealing with wall-clock jitter.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

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
use crate::watch::{HealthSnapshot, StampsSnapshot, TopologySnapshot};

/// Bee's reserve size at depth (`pkg/storer/storer.go`). The reserve
/// fill step uses this as the denominator for the percentage line.
pub const RESERVE_TARGET_CHUNKS: i64 = 65_536;
/// Heuristic peer-bootstrap target. Bee doesn't publish a single
/// "we're done discovering peers" threshold — different versions
/// converge anywhere from 30 to 100. We use 50 as a representative
/// midpoint so the bar reaches Done on a typical mainnet node.
pub const PEER_BOOTSTRAP_TARGET: u64 = 50;
/// Number of consecutive depth observations that must agree for the
/// "kademlia depth stable" step to flip Done. Five ticks at the 1 s
/// cadence ≈ five seconds, long enough to ride out the depth churn
/// during peer bootstrap without dragging a steady node down.
pub const DEPTH_STABILITY_WINDOW: usize = 5;

/// One step of the bootstrap checklist.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepState {
    /// Step hasn't started yet — `░` in the rendered output.
    Pending,
    /// Step in progress, with an integer percentage in `0..=100`. `▒`
    /// in the rendered output.
    InProgress(u32),
    /// Step latched done — `✓`.
    Done,
    /// Insufficient data to classify (snapshots not loaded yet) — `·`.
    Unknown,
}

impl StepState {
    fn glyph(self) -> &'static str {
        let g = theme::active().glyphs;
        match self {
            Self::Pending => g.bar_empty,
            Self::InProgress(_) => g.in_progress,
            Self::Done => g.pass,
            Self::Unknown => g.bullet,
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Pending => theme::active().dim,
            Self::InProgress(_) => theme::active().warn,
            Self::Done => theme::active().pass,
            Self::Unknown => theme::active().dim,
        }
    }
}

/// One row of the warmup checklist.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupStep {
    pub label: &'static str,
    pub state: StepState,
    /// Per-step detail line (e.g. `"487 batches"`, `"depth 8 (5/5
    /// ticks stable)"`). Rendered dimmed under the step glyph.
    pub detail: String,
}

/// Aggregated view fed to renderer and snapshot tests. The elapsed
/// counter is part of the view (as a `Duration`) but tracked in the
/// component because it depends on the component's first observation
/// of `is_warming_up=true`. Tests pass deterministic values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WarmupView {
    /// `true` if Bee currently reports `is_warming_up=true`. After
    /// transition to `false` the component freezes the elapsed
    /// counter but leaves the screen useful as a "definition of
    /// done" view.
    pub is_warming_up: bool,
    /// Wall-clock duration since the component first observed
    /// `is_warming_up=true`. `None` when the snapshot hasn't loaded.
    pub elapsed: Option<Duration>,
    pub steps: Vec<WarmupStep>,
}

pub struct Warmup {
    health_rx: watch::Receiver<HealthSnapshot>,
    stamps_rx: watch::Receiver<StampsSnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    health: HealthSnapshot,
    stamps: StampsSnapshot,
    topology: TopologySnapshot,
    /// Set the first time we see a [`HealthSnapshot`] with
    /// `is_warming_up=true`. Frozen at the moment warmup completes.
    started_at: Option<Instant>,
    /// Elapsed duration captured the moment we observe the warmup-
    /// complete edge — preserved so the screen remains a useful
    /// post-mortem even after `is_warming_up` flips back to false.
    frozen_elapsed: Option<Duration>,
    /// Last few observed kademlia depths. When the window is full and
    /// every entry agrees, the "depth stable" step flips Done.
    depth_history: VecDeque<u8>,
}

impl Warmup {
    pub fn new(
        health_rx: watch::Receiver<HealthSnapshot>,
        stamps_rx: watch::Receiver<StampsSnapshot>,
        topology_rx: watch::Receiver<TopologySnapshot>,
    ) -> Self {
        let health = health_rx.borrow().clone();
        let stamps = stamps_rx.borrow().clone();
        let topology = topology_rx.borrow().clone();
        Self {
            health_rx,
            stamps_rx,
            topology_rx,
            health,
            stamps,
            topology,
            started_at: None,
            frozen_elapsed: None,
            depth_history: VecDeque::with_capacity(DEPTH_STABILITY_WINDOW),
        }
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.stamps = self.stamps_rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
        // Track depth stability window.
        if let Some(t) = &self.topology.topology {
            if self.depth_history.len() == DEPTH_STABILITY_WINDOW {
                self.depth_history.pop_front();
            }
            self.depth_history.push_back(t.depth);
        }
        // Elapsed bookkeeping.
        let warming = self
            .health
            .status
            .as_ref()
            .map(|s| s.is_warming_up)
            .unwrap_or(false);
        if warming {
            if self.started_at.is_none() {
                self.started_at = Some(Instant::now());
            }
            self.frozen_elapsed = None;
        } else if let Some(start) = self.started_at {
            // Edge: warming → not warming. Freeze the elapsed counter
            // once and forget the start instant so a future warmup
            // gets a fresh start.
            if self.frozen_elapsed.is_none() {
                self.frozen_elapsed = Some(Instant::now().saturating_duration_since(start));
            }
            self.started_at = None;
        }
    }

    fn current_elapsed(&self) -> Option<Duration> {
        if let Some(start) = self.started_at {
            Some(Instant::now().saturating_duration_since(start))
        } else {
            self.frozen_elapsed
        }
    }

    fn depth_stable(&self) -> bool {
        if self.depth_history.len() < DEPTH_STABILITY_WINDOW {
            return false;
        }
        let first = match self.depth_history.front() {
            Some(d) => *d,
            None => return false,
        };
        self.depth_history.iter().all(|d| *d == first)
    }

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests so the depth-stable bit and elapsed counter can be
    /// passed in deterministically.
    pub fn view_for(
        health: &HealthSnapshot,
        stamps: &StampsSnapshot,
        topology: &TopologySnapshot,
        elapsed: Option<Duration>,
        depth_stable: bool,
    ) -> WarmupView {
        let is_warming_up = health
            .status
            .as_ref()
            .map(|s| s.is_warming_up)
            .unwrap_or(false);
        let steps = vec![
            postage_step(stamps),
            peers_step(health),
            depth_step(topology, depth_stable),
            reserve_step(health),
            stabilization_step(health),
        ];
        WarmupView {
            is_warming_up,
            elapsed,
            steps,
        }
    }
}

fn postage_step(stamps: &StampsSnapshot) -> WarmupStep {
    if stamps.last_update.is_none() {
        return WarmupStep {
            label: "Postage snapshot loaded",
            state: StepState::Unknown,
            detail: "(awaiting first /stamps poll)".into(),
        };
    }
    let count = stamps.batches.len();
    if count == 0 {
        return WarmupStep {
            label: "Postage snapshot loaded",
            state: StepState::Pending,
            detail: "no batches yet — node may not have any postage attached".into(),
        };
    }
    WarmupStep {
        label: "Postage snapshot loaded",
        state: StepState::Done,
        detail: format!("{count} batch(es)"),
    }
}

fn peers_step(health: &HealthSnapshot) -> WarmupStep {
    let Some(s) = &health.status else {
        return WarmupStep {
            label: "Peer bootstrap",
            state: StepState::Unknown,
            detail: "(awaiting first /status poll)".into(),
        };
    };
    let connected = s.connected_peers as u64;
    let pct = pct_of(connected, PEER_BOOTSTRAP_TARGET);
    let detail = format!("{connected} connected (target ≥ {PEER_BOOTSTRAP_TARGET})");
    if connected >= PEER_BOOTSTRAP_TARGET {
        WarmupStep {
            label: "Peer bootstrap",
            state: StepState::Done,
            detail,
        }
    } else if connected == 0 {
        WarmupStep {
            label: "Peer bootstrap",
            state: StepState::Pending,
            detail,
        }
    } else {
        WarmupStep {
            label: "Peer bootstrap",
            state: StepState::InProgress(pct),
            detail,
        }
    }
}

fn depth_step(topology: &TopologySnapshot, depth_stable: bool) -> WarmupStep {
    let Some(t) = &topology.topology else {
        return WarmupStep {
            label: "Kademlia depth stable",
            state: StepState::Unknown,
            detail: "(awaiting first /topology poll)".into(),
        };
    };
    let detail = if depth_stable {
        format!("depth {} (stable across the observation window)", t.depth)
    } else {
        format!("depth {} (still settling)", t.depth)
    };
    let state = if depth_stable {
        StepState::Done
    } else {
        StepState::InProgress(50)
    };
    WarmupStep {
        label: "Kademlia depth stable",
        state,
        detail,
    }
}

fn reserve_step(health: &HealthSnapshot) -> WarmupStep {
    let Some(s) = &health.status else {
        return WarmupStep {
            label: "Reserve fill",
            state: StepState::Unknown,
            detail: "(awaiting first /status poll)".into(),
        };
    };
    let in_radius = s.reserve_size_within_radius.max(0);
    let pct = pct_of(in_radius as u64, RESERVE_TARGET_CHUNKS as u64);
    let detail = format!("{in_radius} / {RESERVE_TARGET_CHUNKS} in-radius chunks");
    if in_radius >= RESERVE_TARGET_CHUNKS {
        WarmupStep {
            label: "Reserve fill",
            state: StepState::Done,
            detail,
        }
    } else if in_radius == 0 {
        WarmupStep {
            label: "Reserve fill",
            state: StepState::Pending,
            detail,
        }
    } else {
        WarmupStep {
            label: "Reserve fill",
            state: StepState::InProgress(pct),
            detail,
        }
    }
}

fn stabilization_step(health: &HealthSnapshot) -> WarmupStep {
    let Some(s) = &health.status else {
        return WarmupStep {
            label: "Stabilization",
            state: StepState::Unknown,
            detail: "(awaiting first /status poll)".into(),
        };
    };
    if !s.is_warming_up {
        WarmupStep {
            label: "Stabilization",
            state: StepState::Done,
            detail: "Bee reports warmup complete".into(),
        }
    } else {
        WarmupStep {
            label: "Stabilization",
            state: StepState::InProgress(50),
            detail: "Bee still reports is_warming_up=true".into(),
        }
    }
}

fn pct_of(num: u64, denom: u64) -> u32 {
    if denom == 0 {
        return 0;
    }
    let q = num.saturating_mul(100) / denom;
    q.min(100) as u32
}

fn format_elapsed(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3_600 {
        let h = secs / 3_600;
        let m = (secs % 3_600) / 60;
        let s = secs % 60;
        format!("{h}h {m:>2}m {s:>2}s")
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s:>2}s")
    } else {
        format!("{secs}s")
    }
}

impl Component for Warmup {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let elapsed = self.current_elapsed();
        let depth_stable = self.depth_stable();

        let view = Self::view_for(
            &self.health,
            &self.stamps,
            &self.topology,
            elapsed,
            depth_stable,
        );

        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(0),    // step list
            Constraint::Length(1), // footer
        ])
        .split(area);

        // Header
        let elapsed_str = view
            .elapsed
            .map(format_elapsed)
            .unwrap_or_else(|| "—".into());
        let t = theme::active();
        let status_label = if view.is_warming_up {
            Span::styled(
                "warming up",
                Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
            )
        } else if view.elapsed.is_some() {
            Span::styled("complete (post-warmup view)", Style::default().fg(t.pass))
        } else {
            Span::styled("(no /status snapshot yet)", Style::default().fg(t.dim))
        };
        let header_l1 = Line::from(vec![
            Span::styled("WARMUP", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  ·  "),
            status_label,
            Span::raw("  ·  elapsed "),
            Span::styled(elapsed_str, Style::default().fg(t.info)),
        ]);
        let header_l2 = Line::from(Span::styled(
            "  Bee bootstrap is opaque (bee#4746); these checks reconstruct the steps from /status, /stamps, /topology.",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ));
        frame.render_widget(
            Paragraph::new(vec![header_l1, header_l2])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // Step list
        let mut step_lines: Vec<Line> = Vec::new();
        for s in &view.steps {
            let progress_suffix = match s.state {
                StepState::InProgress(pct) => format!("  ({pct}%)"),
                _ => String::new(),
            };
            step_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    s.state.glyph(),
                    Style::default()
                        .fg(s.state.color())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<28}", s.label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(s.detail.clone(), Style::default().fg(t.dim)),
                Span::styled(progress_suffix, Style::default().fg(s.state.color())),
            ]));
        }
        frame.render_widget(Paragraph::new(step_lines), chunks[1]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(
                    "warmup typically takes 25–60 minutes on a fresh mainnet node",
                    Style::default().fg(t.dim),
                ),
            ])),
            chunks[2],
        );

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pct_of_handles_zero_denom() {
        assert_eq!(pct_of(10, 0), 0);
    }

    #[test]
    fn pct_of_clamps_to_100() {
        assert_eq!(pct_of(200, 100), 100);
    }

    #[test]
    fn format_elapsed_unit_thresholds() {
        assert_eq!(format_elapsed(Duration::from_secs(45)), "45s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m  5s");
        assert_eq!(format_elapsed(Duration::from_secs(3_725)), "1h  2m  5s");
    }
}
