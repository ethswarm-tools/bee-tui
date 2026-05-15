//! S5 — Warmup screen (`docs/PLAN.md` § 8.S5).
//!
//! Surfaces the 25–60 minute cold-start opacity (bee#4746). The pure
//! view-computation half (every step's [`StepState`], the percentage
//! math, the elapsed counter as a [`Duration`]) lives in
//! [`bee_cockpit_core::views::warmup`]; this module is the renderer:
//! it polls the three watch channels, keeps the start-time + depth-
//! stability window, picks the glyphs / colours from the active
//! theme, and draws the screen.

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

pub use bee_cockpit_core::views::warmup::{
    DEPTH_STABILITY_WINDOW, PEER_BOOTSTRAP_TARGET, RESERVE_TARGET_CHUNKS, StepState, WarmupStep,
    WarmupView, view_for,
};

use super::Component;
use crate::action::Action;
use crate::theme;
use crate::watch::{HealthSnapshot, StampsSnapshot, TopologySnapshot};

fn step_glyph(s: StepState) -> &'static str {
    let g = theme::active().glyphs;
    match s {
        StepState::Pending => g.bar_empty,
        StepState::InProgress(_) => g.in_progress,
        StepState::Done => g.pass,
        StepState::Unknown => g.bullet,
    }
}

fn step_color(s: StepState) -> Color {
    match s {
        StepState::Pending => theme::active().dim,
        StepState::InProgress(_) => theme::active().warn,
        StepState::Done => theme::active().pass,
        StepState::Unknown => theme::active().dim,
    }
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

    /// Re-export of core's pure view computation so existing callers
    /// of `Warmup::view_for` keep working without an import change.
    pub fn view_for(
        health: &HealthSnapshot,
        stamps: &StampsSnapshot,
        topology: &TopologySnapshot,
        elapsed: Option<Duration>,
        depth_stable: bool,
    ) -> WarmupView {
        view_for(health, stamps, topology, elapsed, depth_stable)
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.stamps = self.stamps_rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
        if let Some(t) = &self.topology.topology {
            if self.depth_history.len() == DEPTH_STABILITY_WINDOW {
                self.depth_history.pop_front();
            }
            self.depth_history.push_back(t.depth);
        }
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

        let view = view_for(
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

        let mut step_lines: Vec<Line> = Vec::new();
        for s in &view.steps {
            let progress_suffix = match s.state {
                StepState::InProgress(pct) => format!("  ({pct}%)"),
                _ => String::new(),
            };
            step_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    step_glyph(s.state),
                    Style::default()
                        .fg(step_color(s.state))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(" "),
                Span::styled(
                    format!("{:<28}", s.label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(s.detail.clone(), Style::default().fg(t.dim)),
                Span::styled(progress_suffix, Style::default().fg(step_color(s.state))),
            ]));
        }
        frame.render_widget(Paragraph::new(step_lines), chunks[1]);

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
    fn format_elapsed_unit_thresholds() {
        assert_eq!(format_elapsed(Duration::from_secs(45)), "45s");
        assert_eq!(format_elapsed(Duration::from_secs(125)), "2m  5s");
        assert_eq!(format_elapsed(Duration::from_secs(3_725)), "1h  2m  5s");
    }
}
