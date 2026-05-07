//! S6 — Peers + bin saturation screen (`docs/PLAN.md` § 8.S6).
//!
//! Driven by [`crate::watch::TopologySnapshot`] (`/topology` @ 5 s).
//! The screen has two halves:
//!
//! 1. **Bin saturation strip** — one row per Kademlia bin (0..=31)
//!    with population vs the bee-go saturation thresholds
//!    (`SaturationPeers=8`, `OverSaturationPeers=18` per
//!    `pkg/topology/kademlia/kademlia.go:54-55`). Bins at or below
//!    the current Kademlia depth flag as Starving when they fall
//!    short of saturation — that's the operator-pain headline this
//!    screen exists to surface (no other tool in the ecosystem
//!    derives this).
//! 2. **Peer table** — flattened view of every connected peer
//!    sourced from each bin's `connectedPeers`. One row per peer
//!    with bin, overlay short, session direction, latency, healthy,
//!    and per-peer reachability.
//!
//! Render delegates to the pure [`Peers::view_for`] so the snapshot
//! tests in `tests/s6_peers_view.rs` pin every classification edge
//! without launching a TUI.

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
use crate::watch::TopologySnapshot;

use bee::debug::{BinInfo, PeerInfo, Topology};

/// Kademlia bins per Bee build.
pub const BIN_COUNT: usize = 32;
/// `pkg/topology/kademlia/kademlia.go:54` — saturation threshold.
/// Connected peer counts below this in a relevant bin (bin ≤ depth)
/// are reported as Starving.
pub const SATURATION_PEERS: u64 = 8;
/// `pkg/topology/kademlia/kademlia.go:55` — over-saturation threshold.
/// Connected peer counts above this in any bin are reported as Over.
pub const OVER_SATURATION_PEERS: u64 = 18;
/// Relaxation factor for "far" bins. Bins more than this many positions
/// past the kademlia depth are expected to be sparse and don't flag as
/// Starving even if their connected count is below saturation.
const FAR_BIN_RELAXATION: u8 = 4;

/// Tri-state bin saturation classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinSaturation {
    /// `connected == 0` and the bin is far from depth — expected to
    /// be empty.
    Empty,
    /// `connected < SATURATION_PEERS` for a bin at or near the depth.
    /// Operator should add peers (manual `connect`, more uptime).
    Starving,
    /// Connected count is in the safe band `[8, 18]`.
    Healthy,
    /// Connected count exceeds `OVER_SATURATION_PEERS`. Bee will trim
    /// oldest entries; harmless but unusual for distant bins.
    Over,
}

impl BinSaturation {
    fn color(self) -> Color {
        match self {
            Self::Empty => Color::DarkGray,
            Self::Starving => Color::Red,
            Self::Healthy => Color::Green,
            Self::Over => Color::Yellow,
        }
    }
    fn label(self) -> &'static str {
        match self {
            Self::Empty => "—",
            Self::Starving => "✗ STARVING",
            Self::Healthy => "✓",
            Self::Over => "⚠ over",
        }
    }
}

/// One row of the bin saturation strip.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BinStripRow {
    pub bin: u8,
    pub population: u64,
    pub connected: u64,
    pub status: BinSaturation,
    /// `true` if this bin is at or below the current kademlia depth —
    /// the only bins where Starving carries an operator alert. Far
    /// bins with low population are normal.
    pub is_relevant: bool,
}

/// One row of the peer table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerRow {
    pub bin: u8,
    pub peer_short: String,
    /// `"in"` / `"out"` from the per-peer session direction. `"?"` if
    /// metrics not yet populated.
    pub direction: &'static str,
    /// Latency formatted as `"Xms"` from the EWMA value (Bee returns
    /// nanoseconds). `"—"` if metrics absent.
    pub latency: String,
    pub healthy: bool,
    /// Per-peer reachability string from MetricSnapshotView. Empty if
    /// metrics absent.
    pub reachability: String,
}

/// Aggregated view fed to the renderer and snapshot tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeersView {
    pub bins: Vec<BinStripRow>,
    pub peers: Vec<PeerRow>,
    pub depth: u8,
    pub population: i64,
    pub connected: i64,
    pub reachability: String,
    pub network_availability: String,
    /// Number of connected light-node peers, separate from the
    /// 32-bin breakdown.
    pub light_connected: u64,
}

pub struct Peers {
    rx: watch::Receiver<TopologySnapshot>,
    snapshot: TopologySnapshot,
}

impl Peers {
    pub fn new(rx: watch::Receiver<TopologySnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self { rx, snapshot }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
    }

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests in `tests/s6_peers_view.rs`.
    pub fn view_for(snap: &TopologySnapshot) -> Option<PeersView> {
        let t = snap.topology.as_ref()?;
        let bins = bin_strip_rows(t);
        let peers = peer_rows(t);
        Some(PeersView {
            bins,
            peers,
            depth: t.depth,
            population: t.population,
            connected: t.connected,
            reachability: t.reachability.clone(),
            network_availability: t.network_availability.clone(),
            light_connected: t.light_nodes.connected,
        })
    }
}

fn bin_strip_rows(t: &Topology) -> Vec<BinStripRow> {
    t.bins
        .iter()
        .enumerate()
        .map(|(i, b)| {
            let bin = i as u8;
            let is_relevant = bin <= t.depth.saturating_add(FAR_BIN_RELAXATION);
            BinStripRow {
                bin,
                population: b.population,
                connected: b.connected,
                status: classify_bin(b, bin, t.depth),
                is_relevant,
            }
        })
        .collect()
}

fn classify_bin(b: &BinInfo, bin: u8, depth: u8) -> BinSaturation {
    if b.connected > OVER_SATURATION_PEERS {
        return BinSaturation::Over;
    }
    if b.connected >= SATURATION_PEERS {
        return BinSaturation::Healthy;
    }
    // Below saturation: starving only if this bin is at or near the
    // current depth. Far bins are expected to be sparse.
    if bin <= depth.saturating_add(FAR_BIN_RELAXATION) {
        BinSaturation::Starving
    } else {
        BinSaturation::Empty
    }
}

fn peer_rows(t: &Topology) -> Vec<PeerRow> {
    let mut out: Vec<PeerRow> = Vec::new();
    for (i, b) in t.bins.iter().enumerate() {
        let bin = i as u8;
        for p in &b.connected_peers {
            out.push(make_peer_row(bin, p));
        }
    }
    // Stable order: bin asc, then peer overlay asc — so the table
    // doesn't shuffle every poll tick when populations don't change.
    out.sort_by(|a, b| {
        a.bin
            .cmp(&b.bin)
            .then_with(|| a.peer_short.cmp(&b.peer_short))
    });
    out
}

fn make_peer_row(bin: u8, p: &PeerInfo) -> PeerRow {
    let peer_short = short_overlay(&p.address);
    let (direction, latency, healthy, reachability) = match &p.metrics {
        Some(m) => {
            let direction = match m.session_connection_direction.as_str() {
                "inbound" => "in",
                "outbound" => "out",
                _ => "?",
            };
            let latency_ms = m.latency_ewma.max(0) as f64 / 1_000_000.0;
            let latency = if m.latency_ewma > 0 {
                format!("{latency_ms:.0}ms")
            } else {
                "—".into()
            };
            (direction, latency, m.healthy, m.reachability.clone())
        }
        None => ("?", "—".into(), false, String::new()),
    };
    PeerRow {
        bin,
        peer_short,
        direction,
        latency,
        healthy,
        reachability,
    }
}

fn short_overlay(s: &str) -> String {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.len() > 10 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
    }
}

impl Component for Peers {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Length(20), // bin strip (32 lines + header)
            Constraint::Min(0),    // peer table
            Constraint::Length(1), // footer
        ])
        .split(area);

        // Header
        let header_l1 = Line::from(vec![Span::styled(
            "PEERS / TOPOLOGY",
            Style::default().add_modifier(Modifier::BOLD),
        )]);
        let mut header_l2 = Vec::new();
        if let Some(err) = &self.snapshot.last_error {
            header_l2.push(Span::styled(
                format!("error: {err}"),
                Style::default().fg(Color::Red),
            ));
        } else if !self.snapshot.is_loaded() {
            header_l2.push(Span::styled(
                "loading…",
                Style::default().fg(Color::DarkGray),
            ));
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let view = match Self::view_for(&self.snapshot) {
            Some(v) => v,
            None => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "  topology not loaded yet",
                        Style::default()
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    )),
                    chunks[1],
                );
                return Ok(());
            }
        };

        // Bin strip
        let mut strip_lines: Vec<Line> = vec![
            Line::from(vec![
                Span::styled(
                    format!(
                        "  depth {} · connected {} / known {} · reachability {} · net {}",
                        view.depth,
                        view.connected,
                        view.population,
                        if view.reachability.is_empty() {
                            "?".to_string()
                        } else {
                            view.reachability.clone()
                        },
                        if view.network_availability.is_empty() {
                            "?".to_string()
                        } else {
                            view.network_availability.clone()
                        },
                    ),
                    Style::default().fg(Color::DarkGray),
                ),
            ]),
            Line::from(Span::styled(
                "  BIN  POP  CONN  BAR              STATUS",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::BOLD),
            )),
        ];
        for r in &view.bins {
            // Skip far empty bins past the relaxation window — too much
            // noise to render all 32 when only the first ~12 matter.
            if !r.is_relevant && r.population == 0 {
                continue;
            }
            let bar = bin_bar(r.connected as usize, 12);
            strip_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:>3} ", r.bin),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:>4} ", r.population)),
                Span::raw(format!("{:>4}  ", r.connected)),
                Span::styled(format!("{bar:<14}"), Style::default().fg(r.status.color())),
                Span::raw(" "),
                Span::styled(
                    r.status.label(),
                    Style::default()
                        .fg(r.status.color())
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
        if view.light_connected > 0 {
            strip_lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!(" light  —  {}    (separate from main bins)", view.light_connected),
                    Style::default().fg(Color::DarkGray),
                ),
            ]));
        }
        frame.render_widget(
            Paragraph::new(strip_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        // Peer table
        let mut peer_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  BIN  PEER          DIR  LATENCY   HEALTHY  REACHABILITY",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::BOLD),
        ))];
        if view.peers.is_empty() {
            peer_lines.push(Line::from(Span::styled(
                "  (no connected peers reported)",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            )));
        } else {
            for p in &view.peers {
                let healthy_glyph = if p.healthy { "✓" } else { "✗" };
                let healthy_style = if p.healthy {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Red)
                };
                peer_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::raw(format!("{:>3}  ", p.bin)),
                    Span::raw(format!("{:<13} ", p.peer_short)),
                    Span::raw(format!("{:<4} ", p.direction)),
                    Span::raw(format!("{:<8}  ", p.latency)),
                    Span::styled(format!("{healthy_glyph:<7} "), healthy_style),
                    Span::raw(p.reachability.clone()),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(peer_lines), chunks[2]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(
                    format!("thresholds: {SATURATION_PEERS} saturate · {OVER_SATURATION_PEERS} over"),
                    Style::default().fg(Color::DarkGray),
                ),
            ])),
            chunks[3],
        );

        Ok(())
    }
}

/// Width-bounded ASCII bar showing connected count, capped at
/// [`OVER_SATURATION_PEERS`] for visual scale.
fn bin_bar(connected: usize, width: usize) -> String {
    let scale = OVER_SATURATION_PEERS as usize;
    let filled = connected.min(scale) * width / scale.max(1);
    let mut bar = String::with_capacity(width);
    for _ in 0..filled.min(width) {
        bar.push('▇');
    }
    for _ in filled.min(width)..width {
        bar.push('░');
    }
    bar
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bin(population: u64, connected: u64) -> BinInfo {
        BinInfo {
            population,
            connected,
            ..BinInfo::default()
        }
    }

    #[test]
    fn classify_below_saturation_in_relevant_bin_is_starving() {
        // bin 4, depth 8 → bin <= depth + 4 = 12 → relevant → starving.
        assert_eq!(
            classify_bin(&bin(5, 3), 4, 8),
            BinSaturation::Starving
        );
    }

    #[test]
    fn classify_below_saturation_in_far_bin_is_empty() {
        // bin 20, depth 8 → bin > depth + 4 = 12 → far → empty.
        assert_eq!(
            classify_bin(&bin(0, 0), 20, 8),
            BinSaturation::Empty
        );
    }

    #[test]
    fn classify_in_safe_band_is_healthy() {
        assert_eq!(classify_bin(&bin(15, 12), 4, 8), BinSaturation::Healthy);
        assert_eq!(
            classify_bin(&bin(8, SATURATION_PEERS), 4, 8),
            BinSaturation::Healthy
        );
    }

    #[test]
    fn classify_over_threshold_is_over() {
        assert_eq!(
            classify_bin(&bin(25, OVER_SATURATION_PEERS + 1), 4, 8),
            BinSaturation::Over
        );
    }

    #[test]
    fn short_overlay_truncates() {
        let s = short_overlay("0xabcdef0123456789abcdef0123456789");
        assert!(s.contains('…'));
        assert!(s.starts_with("abcdef"));
    }

    #[test]
    fn bin_bar_caps_at_oversaturation() {
        let bar_full = bin_bar(50, 12);
        assert_eq!(bar_full, "▇".repeat(12));
        let bar_empty = bin_bar(0, 12);
        assert_eq!(bar_empty, "░".repeat(12));
    }
}
