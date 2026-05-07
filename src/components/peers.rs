//! S6 — Peers + bin saturation screen (`docs/PLAN.md` § 8.S6).
//!
//! Driven by [`crate::watch::TopologySnapshot`] (`/topology` @ 5 s).
//! The screen has three states, switched by key:
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
//!    and per-peer reachability. Selection (↑↓/jk) highlights one
//!    row; `↵` opens the per-peer drill pane.
//! 3. **Peer drill** — fans out four parallel fetches (`peer_balance`,
//!    `peer_cheques`, `peer_settlement`, `ping_peer`) for the
//!    selected peer and renders the results inline. `Esc` returns
//!    to the table.
//!
//! Pure compute paths (`view_for`, `compute_peer_drill_view`) are
//! exposed for the snapshot tests in `tests/s6_peers_view.rs` and
//! `tests/s6_peers_drill.rs`.

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
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::TopologySnapshot;

use bee::debug::{Balance, BinInfo, PeerCheques, PeerInfo, Settlement, Topology};

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
            Self::Empty => theme::active().dim,
            Self::Starving => theme::active().fail,
            Self::Healthy => theme::active().pass,
            Self::Over => theme::active().warn,
        }
    }
    fn label(self) -> String {
        let g = theme::active().glyphs;
        match self {
            Self::Empty => g.em_dash.to_string(),
            Self::Starving => format!("{} STARVING", g.fail),
            Self::Healthy => g.pass.to_string(),
            Self::Over => format!("{} over", g.warn),
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
    /// Full overlay address (un-shortened) — needed for drill
    /// fetches. Short version is just for display.
    pub peer_full: String,
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

/// Per-field outcome of a peer drill fetch. Each endpoint can fail
/// independently — render each row with what it has.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DrillField<T: Clone + PartialEq + Eq> {
    Ok(T),
    Err(String),
}

/// Aggregated drill view for the per-peer pane. Pure — computed
/// from the four endpoint results without any I/O.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerDrillView {
    pub peer_overlay: String,
    pub bin: Option<u8>,
    /// Settlement balance — positive = peer owes us, negative = we
    /// owe peer. Pre-formatted as `"BZZ X.XXXX"` with sign.
    pub balance: DrillField<String>,
    /// Round-trip time as Bee reports it (Go duration string,
    /// e.g. `"5.0018ms"`).
    pub ping: DrillField<String>,
    /// Cumulative settlement received from this peer (BZZ).
    pub settlement_received: DrillField<String>,
    /// Cumulative settlement sent to this peer (BZZ).
    pub settlement_sent: DrillField<String>,
    /// Last received cheque payout (BZZ) or `None` if no cheques.
    pub last_received_cheque: DrillField<Option<String>>,
    /// Last sent cheque payout (BZZ) or `None` if no cheques.
    pub last_sent_cheque: DrillField<Option<String>>,
}

/// Bundle of the four endpoint results that feeds
/// [`Peers::compute_peer_drill_view`]. Each field can fail
/// independently.
#[derive(Debug, Clone)]
pub struct PeerDrillFetch {
    pub balance: std::result::Result<Balance, String>,
    pub cheques: std::result::Result<PeerCheques, String>,
    pub settlement: std::result::Result<Settlement, String>,
    pub ping: std::result::Result<String, String>,
}

/// Drill-pane state machine. `Idle` keeps the regular table
/// rendered; the other variants replace the peer table with the
/// drill view.
#[derive(Debug, Clone)]
pub enum DrillState {
    Idle,
    Loading { peer: String, bin: Option<u8> },
    Loaded { view: PeerDrillView },
}

type DrillFetchResult = (String, PeerDrillFetch);

pub struct Peers {
    client: Arc<ApiClient>,
    rx: watch::Receiver<TopologySnapshot>,
    snapshot: TopologySnapshot,
    selected: usize,
    /// Top row of the visible window in the peer table. Drives both
    /// the `Paragraph::scroll` offset and the right-edge scrollbar
    /// state. Updated lazily inside `draw_peer_table` to match the
    /// area height we get at render time.
    scroll_offset: usize,
    drill: DrillState,
    fetch_tx: mpsc::UnboundedSender<DrillFetchResult>,
    fetch_rx: mpsc::UnboundedReceiver<DrillFetchResult>,
}

impl Peers {
    pub fn new(client: Arc<ApiClient>, rx: watch::Receiver<TopologySnapshot>) -> Self {
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
        let n = self.peer_rows_cached().len();
        if n == 0 {
            self.selected = 0;
        } else if self.selected >= n {
            self.selected = n - 1;
        }
    }

    fn peer_rows_cached(&self) -> Vec<PeerRow> {
        Self::view_for(&self.snapshot)
            .map(|v| v.peers)
            .unwrap_or_default()
    }

    /// Drain any drill fetches that completed since the last tick.
    /// Late results from a since-cancelled drill (operator hit Esc
    /// then Enter on a different peer before all four endpoints
    /// came back) are dropped silently — `drill` already moved on.
    fn drain_fetches(&mut self) {
        while let Ok((peer, fetch)) = self.fetch_rx.try_recv() {
            let pending_peer = match &self.drill {
                DrillState::Loading { peer: p, .. } => p.clone(),
                _ => continue, // user moved on; ignore
            };
            if pending_peer != peer {
                continue;
            }
            let bin = match &self.drill {
                DrillState::Loading { bin, .. } => *bin,
                _ => None,
            };
            let view = Self::compute_peer_drill_view(&peer, bin, &fetch);
            self.drill = DrillState::Loaded { view };
        }
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

    /// Pure compute path for the per-peer drill pane. Each field is
    /// derived independently so a single failed endpoint doesn't
    /// blank the view — operators still see whatever did come back.
    pub fn compute_peer_drill_view(
        peer: &str,
        bin: Option<u8>,
        fetch: &PeerDrillFetch,
    ) -> PeerDrillView {
        let balance = match &fetch.balance {
            Ok(b) => DrillField::Ok(format_plur_signed(&b.balance)),
            Err(e) => DrillField::Err(e.clone()),
        };
        let ping = match &fetch.ping {
            Ok(s) => DrillField::Ok(s.clone()),
            Err(e) => DrillField::Err(e.clone()),
        };
        let (settlement_received, settlement_sent) = match &fetch.settlement {
            Ok(s) => (
                DrillField::Ok(format_opt_plur(s.received.as_ref())),
                DrillField::Ok(format_opt_plur(s.sent.as_ref())),
            ),
            Err(e) => (DrillField::Err(e.clone()), DrillField::Err(e.clone())),
        };
        let (last_received_cheque, last_sent_cheque) = match &fetch.cheques {
            Ok(c) => (
                DrillField::Ok(
                    c.last_received
                        .as_ref()
                        .map(|q| format_opt_plur(q.payout.as_ref())),
                ),
                DrillField::Ok(
                    c.last_sent
                        .as_ref()
                        .map(|q| format_opt_plur(q.payout.as_ref())),
                ),
            ),
            Err(e) => (DrillField::Err(e.clone()), DrillField::Err(e.clone())),
        };
        PeerDrillView {
            peer_overlay: peer.to_string(),
            bin,
            balance,
            ping,
            settlement_received,
            settlement_sent,
            last_received_cheque,
            last_sent_cheque,
        }
    }

    /// Spawn parallel fetches of the four per-peer endpoints. No-op
    /// if the peer table is empty or a fetch is already in flight
    /// for the same peer.
    fn maybe_start_drill(&mut self) {
        let peers = self.peer_rows_cached();
        if peers.is_empty() {
            return;
        }
        let i = self.selected.min(peers.len() - 1);
        let row = &peers[i];
        let peer = row.peer_full.clone();
        let bin = Some(row.bin);
        if let DrillState::Loading { peer: pending, .. } = &self.drill {
            if *pending == peer {
                return;
            }
        }
        let client = self.client.clone();
        let tx = self.fetch_tx.clone();
        let peer_for_task = peer.clone();
        tokio::spawn(async move {
            let bee = client.bee();
            let debug = bee.debug();
            // Fan out all four requests in parallel — they're all
            // small and independent. tokio::join! returns once every
            // future has resolved (even if some errored), so the
            // operator sees the full picture in one update rather
            // than four flickers.
            let (balance, cheques, settlement, ping) = tokio::join!(
                debug.peer_balance(&peer_for_task),
                debug.peer_cheques(&peer_for_task),
                debug.peer_settlement(&peer_for_task),
                debug.ping_peer(&peer_for_task),
            );
            let fetch = PeerDrillFetch {
                balance: balance.map_err(|e| e.to_string()),
                cheques: cheques.map_err(|e| e.to_string()),
                settlement: settlement.map_err(|e| e.to_string()),
                ping: ping.map_err(|e| e.to_string()),
            };
            let _ = tx.send((peer_for_task, fetch));
        });
        self.drill = DrillState::Loading { peer, bin };
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
    let peer_full = p.address.trim_start_matches("0x").to_string();
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
        peer_full,
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

/// Format a PLUR `BigInt` as a signed `"BZZ X.XXXX"` string. Mirrors
/// the helper in `swap.rs` — duplicated here to keep peers.rs free
/// of cross-module coupling and avoid widening the public API.
fn format_plur_signed(plur: &BigInt) -> String {
    let zero = BigInt::from(0);
    let neg = plur < &zero;
    let abs = if neg { -plur.clone() } else { plur.clone() };
    let scale = BigInt::from(10u64).pow(16);
    let whole = &abs / &scale;
    let frac = &abs % &scale;
    let frac_4 = &frac / BigInt::from(10u64).pow(12);
    let sign = if neg { "-" } else { "+" };
    format!("{sign}BZZ {whole}.{frac_4:0>4}")
}

fn format_opt_plur(plur: Option<&BigInt>) -> String {
    match plur {
        Some(p) => format_plur_signed(p).trim_start_matches('+').to_string(),
        None => "—".to_string(),
    }
}

impl Component for Peers {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
            self.drain_fetches();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        // Drill mode: Esc dismisses; everything else is ignored
        // (drill is read-only). Selection keys still work in Idle so
        // the user can navigate before hitting Enter.
        if matches!(
            self.drill,
            DrillState::Loaded { .. } | DrillState::Loading { .. }
        ) && matches!(key.code, KeyCode::Esc)
        {
            self.drill = DrillState::Idle;
            return Ok(None);
        }
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                let n = self.peer_rows_cached().len();
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
            Constraint::Length(3),  // header
            Constraint::Length(20), // bin strip (32 lines + header)
            Constraint::Min(0),     // peer table OR drill
            Constraint::Length(1),  // footer
        ])
        .split(area);

        // Header
        let mut header_l1 = vec![Span::styled(
            "PEERS / TOPOLOGY",
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if let DrillState::Loaded { view } = &self.drill {
            header_l1.push(Span::raw(format!(
                "   · drill {}",
                short_overlay(&view.peer_overlay)
            )));
        } else if let DrillState::Loading { peer, .. } = &self.drill {
            header_l1.push(Span::raw(format!(
                "   · drill {} (loading)",
                short_overlay(peer)
            )));
        }
        let header_l1 = Line::from(header_l1);
        let mut header_l2 = Vec::new();
        let t = theme::active();
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

        let view = match Self::view_for(&self.snapshot) {
            Some(v) => v,
            None => {
                frame.render_widget(
                    Paragraph::new(Span::styled(
                        "  topology not loaded yet",
                        Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                    )),
                    chunks[1],
                );
                return Ok(());
            }
        };

        // Bin strip
        let mut strip_lines: Vec<Line> = vec![
            Line::from(vec![Span::styled(
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
                Style::default().fg(t.dim),
            )]),
            Line::from(Span::styled(
                "  BIN  POP  CONN  BAR              STATUS",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
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
                    format!(
                        " light  —  {}    (separate from main bins)",
                        view.light_connected
                    ),
                    Style::default().fg(t.dim),
                ),
            ]));
        }
        frame.render_widget(
            Paragraph::new(strip_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        // Peer table OR drill
        match &self.drill {
            DrillState::Idle => self.draw_peer_table(frame, chunks[2], &view.peers),
            DrillState::Loading { peer, .. } => {
                let msg = Line::from(vec![
                    Span::raw("  fetching peer drill for "),
                    Span::styled(
                        short_overlay(peer),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("…   (Esc cancel)"),
                ]);
                frame.render_widget(Paragraph::new(msg), chunks[2]);
            }
            DrillState::Loaded { view: drill_view } => {
                self.draw_peer_drill(frame, chunks[2], drill_view);
            }
        }

        // Footer
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
                Span::styled(
                    format!(
                        "thresholds: {SATURATION_PEERS} saturate · {OVER_SATURATION_PEERS} over"
                    ),
                    Style::default().fg(t.dim),
                ),
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

impl Peers {
    fn draw_peer_table(&mut self, frame: &mut Frame, area: Rect, peers: &[PeerRow]) {
        use ratatui::layout::{Constraint, Layout};

        let t = theme::active();

        // Two-row split: pinned column header + scrollable body. The
        // header doesn't scroll out from under the cursor, which is
        // what operators expect after using k9s / lazygit.
        let table_chunks =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(area);

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "   BIN  PEER          DIR  LATENCY   HEALTHY  REACHABILITY",
                Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
            ))),
            table_chunks[0],
        );

        if peers.is_empty() {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    "   (no connected peers reported)",
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ))),
                table_chunks[1],
            );
            return;
        }

        let mut peer_lines: Vec<Line> = Vec::with_capacity(peers.len());
        for (i, p) in peers.iter().enumerate() {
            let g = theme::active().glyphs;
            let healthy_glyph = if p.healthy { g.pass } else { g.fail };
            let healthy_style = if p.healthy {
                Style::default().fg(t.pass)
            } else {
                Style::default().fg(t.fail)
            };
            let cursor = if i == self.selected {
                format!("{} ", t.glyphs.cursor)
            } else {
                "  ".to_string()
            };
            peer_lines.push(Line::from(vec![
                Span::styled(
                    cursor,
                    Style::default()
                        .fg(if i == self.selected { t.accent } else { t.dim })
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:>3}  ", p.bin)),
                Span::raw(format!("{:<13} ", p.peer_short)),
                Span::raw(format!("{:<4} ", p.direction)),
                Span::raw(format!("{:<8}  ", p.latency)),
                Span::styled(format!("{healthy_glyph:<7} "), healthy_style),
                Span::raw(p.reachability.clone()),
            ]));
        }

        let body = table_chunks[1];
        let visible_rows = body.height as usize;
        self.scroll_offset = super::scroll::clamp_scroll(
            self.selected,
            self.scroll_offset,
            visible_rows,
            peer_lines.len(),
        );
        frame.render_widget(
            Paragraph::new(peer_lines.clone()).scroll((self.scroll_offset as u16, 0)),
            body,
        );
        super::scroll::render_scrollbar(
            frame,
            body,
            self.scroll_offset,
            visible_rows,
            peer_lines.len(),
        );
    }

    fn draw_peer_drill(&self, frame: &mut Frame, area: Rect, view: &PeerDrillView) {
        let t = theme::active();
        let mut lines: Vec<Line> = Vec::new();
        let bin_label = view
            .bin
            .map(|b| format!("bin {b}"))
            .unwrap_or_else(|| "bin ?".into());
        lines.push(Line::from(vec![
            Span::raw("  peer "),
            Span::styled(
                view.peer_overlay.clone(),
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::raw("   "),
            Span::styled(bin_label, Style::default().fg(t.dim)),
        ]));
        lines.push(Line::from(""));
        lines.push(drill_field_line("balance        ", &view.balance, t));
        lines.push(drill_field_line("ping rtt       ", &view.ping, t));
        lines.push(drill_field_line(
            "settle recv    ",
            &view.settlement_received,
            t,
        ));
        lines.push(drill_field_line(
            "settle sent    ",
            &view.settlement_sent,
            t,
        ));
        lines.push(drill_field_optional_line(
            "cheque last in ",
            &view.last_received_cheque,
            t,
        ));
        lines.push(drill_field_optional_line(
            "cheque last out",
            &view.last_sent_cheque,
            t,
        ));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  (Esc to dismiss · figures are point-in-time, not live-updating)",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        )));
        frame.render_widget(Paragraph::new(lines), area);
    }
}

fn drill_field_line(label: &str, field: &DrillField<String>, t: &theme::Theme) -> Line<'static> {
    match field {
        DrillField::Ok(v) => Line::from(vec![
            Span::raw("  "),
            Span::styled(label.to_string(), Style::default().fg(t.dim)),
            Span::raw("  "),
            Span::styled(v.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        DrillField::Err(e) => Line::from(vec![
            Span::raw("  "),
            Span::styled(label.to_string(), Style::default().fg(t.dim)),
            Span::raw("  "),
            Span::styled(format!("error: {e}"), Style::default().fg(t.fail)),
        ]),
    }
}

fn drill_field_optional_line(
    label: &str,
    field: &DrillField<Option<String>>,
    t: &theme::Theme,
) -> Line<'static> {
    match field {
        DrillField::Ok(Some(v)) => Line::from(vec![
            Span::raw("  "),
            Span::styled(label.to_string(), Style::default().fg(t.dim)),
            Span::raw("  "),
            Span::styled(v.clone(), Style::default().add_modifier(Modifier::BOLD)),
        ]),
        DrillField::Ok(None) => Line::from(vec![
            Span::raw("  "),
            Span::styled(label.to_string(), Style::default().fg(t.dim)),
            Span::raw("  "),
            Span::styled(
                "(no cheque yet)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            ),
        ]),
        DrillField::Err(e) => Line::from(vec![
            Span::raw("  "),
            Span::styled(label.to_string(), Style::default().fg(t.dim)),
            Span::raw("  "),
            Span::styled(format!("error: {e}"), Style::default().fg(t.fail)),
        ]),
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
        assert_eq!(classify_bin(&bin(5, 3), 4, 8), BinSaturation::Starving);
    }

    #[test]
    fn classify_below_saturation_in_far_bin_is_empty() {
        // bin 20, depth 8 → bin > depth + 4 = 12 → far → empty.
        assert_eq!(classify_bin(&bin(0, 0), 20, 8), BinSaturation::Empty);
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

    #[test]
    fn drill_view_all_ok() {
        // 5 × 10^14 PLUR = 0.05 BZZ — typical cheque magnitude.
        let cheque_in = bee::debug::Cheque {
            beneficiary: "0x".into(),
            chequebook: "0x".into(),
            payout: Some(BigInt::from(500_000_000_000_000u64)),
        };
        let fetch = PeerDrillFetch {
            balance: Ok(Balance {
                peer: "abcd".into(),
                balance: BigInt::from(123_400_000_000_000_000i64),
            }),
            cheques: Ok(PeerCheques {
                peer: "abcd".into(),
                last_received: Some(cheque_in),
                last_sent: None,
            }),
            settlement: Ok(Settlement {
                peer: "abcd".into(),
                received: Some(BigInt::from(900_000_000_000_000_000u64)),
                sent: Some(BigInt::from(100_000_000_000_000_000u64)),
            }),
            ping: Ok("4.21ms".into()),
        };
        let view = Peers::compute_peer_drill_view("abcd1234", Some(7), &fetch);
        assert_eq!(view.bin, Some(7));
        match &view.balance {
            DrillField::Ok(s) => assert!(s.contains("BZZ")),
            _ => panic!("expected ok balance"),
        }
        match &view.last_received_cheque {
            DrillField::Ok(Some(s)) => assert!(s.contains("0.0500")),
            _ => panic!("expected received cheque payout"),
        }
        match &view.last_sent_cheque {
            DrillField::Ok(None) => {}
            _ => panic!("expected None for sent cheque"),
        }
    }

    #[test]
    fn drill_view_partial_failure_keeps_other_fields() {
        // Settlement endpoint failed, the rest succeeded — drill view
        // must still surface what came back rather than blanking.
        let fetch = PeerDrillFetch {
            balance: Ok(Balance {
                peer: "x".into(),
                balance: BigInt::from(0),
            }),
            cheques: Err("404".into()),
            settlement: Err("503 Node is syncing".into()),
            ping: Ok("12ms".into()),
        };
        let view = Peers::compute_peer_drill_view("xxxx", None, &fetch);
        assert!(matches!(view.balance, DrillField::Ok(_)));
        assert!(matches!(view.ping, DrillField::Ok(_)));
        assert!(matches!(view.settlement_received, DrillField::Err(_)));
        assert!(matches!(view.last_received_cheque, DrillField::Err(_)));
    }

    #[test]
    fn format_plur_signed_handles_zero_and_negative() {
        assert_eq!(format_plur_signed(&BigInt::from(0)), "+BZZ 0.0000");
        assert_eq!(
            format_plur_signed(&BigInt::from(-5_000_000_000_000_000i64)),
            "-BZZ 0.5000"
        );
    }
}
