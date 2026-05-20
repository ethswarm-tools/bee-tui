//! S6 — Peers + bin saturation screen (`docs/PLAN.md` § 8.S6).
//! Pure view-data half lives in [`bee_cockpit_core::views::peers`];
//! this module owns the API client handle, the drill-pane fetch
//! channels (4-way parallel join), the cursor + scroll offset, and
//! the ratatui draw path.

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

pub use bee_cockpit_core::views::peers::{
    BIN_COUNT, BatchCommitmentCell, BinSaturation, BinStripRow, DrillField, FAR_BIN_RELAXATION,
    OVER_SATURATION_PEERS, PeerDrillFetch, PeerDrillView, PeerRow, PeersView, SATURATION_PEERS,
    SaturationSummary, compute_peer_drill_view, format_thousands, short_overlay, view_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::TopologySnapshot;

fn bin_color(s: BinSaturation) -> Color {
    match s {
        BinSaturation::Empty => theme::active().dim,
        BinSaturation::Starving => theme::active().fail,
        BinSaturation::Healthy => theme::active().pass,
        BinSaturation::Over => theme::active().warn,
    }
}

fn bin_label(s: BinSaturation) -> String {
    let g = theme::active().glyphs;
    match s {
        BinSaturation::Empty => g.em_dash.to_string(),
        BinSaturation::Starving => format!("{} STARVING", g.fail),
        BinSaturation::Healthy => g.pass.to_string(),
        BinSaturation::Over => format!("{} over", g.warn),
    }
}

/// Drill-pane state machine. `Loaded` boxes the view because
/// PeerDrillView is substantially larger than the other variants.
#[derive(Debug, Clone)]
pub enum DrillState {
    Idle,
    Loading { peer: String, bin: Option<u8> },
    Loaded { view: Box<PeerDrillView> },
}

type DrillFetchResult = (String, PeerDrillFetch);

pub struct Peers {
    client: Arc<ApiClient>,
    rx: watch::Receiver<TopologySnapshot>,
    snapshot: TopologySnapshot,
    selected: usize,
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

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Peers::view_for` call sites resolve.
    pub fn view_for(snap: &TopologySnapshot) -> Option<PeersView> {
        view_for(snap)
    }

    /// Re-export of core's pure drill-view computation as an inherent
    /// function so existing `Peers::compute_peer_drill_view` call
    /// sites (snapshot test files) resolve.
    pub fn compute_peer_drill_view(
        peer: &str,
        bin: Option<u8>,
        fetch: &PeerDrillFetch,
    ) -> PeerDrillView {
        compute_peer_drill_view(peer, bin, fetch)
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
        view_for(&self.snapshot)
            .map(|v| v.peers)
            .unwrap_or_default()
    }

    fn drain_fetches(&mut self) {
        while let Ok((peer, fetch)) = self.fetch_rx.try_recv() {
            let pending_peer = match &self.drill {
                DrillState::Loading { peer: p, .. } => p.clone(),
                _ => continue,
            };
            if pending_peer != peer {
                continue;
            }
            let bin = match &self.drill {
                DrillState::Loading { bin, .. } => *bin,
                _ => None,
            };
            let view = compute_peer_drill_view(&peer, bin, &fetch);
            self.drill = DrillState::Loaded {
                view: Box::new(view),
            };
        }
    }

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
            let (balance, cheques, settlement, ping, status_peers, local_status) = tokio::join!(
                debug.peer_balance(&peer_for_task),
                debug.peer_cheques(&peer_for_task),
                debug.peer_settlement(&peer_for_task),
                debug.ping_peer(&peer_for_task),
                debug.status_peers(),
                debug.status(),
            );
            let peer_status = status_peers
                .map(|rows| {
                    rows.into_iter()
                        .find(|r| peer_for_task.contains(&r.status.overlay))
                })
                .map_err(|e| e.to_string());
            let fetch = PeerDrillFetch {
                balance: balance.map_err(|e| e.to_string()),
                cheques: cheques.map_err(|e| e.to_string()),
                settlement: settlement.map_err(|e| e.to_string()),
                ping: ping.map_err(|e| e.to_string()),
                peer_status,
                local_status: local_status.map_err(|e| e.to_string()),
            };
            let _ = tx.send((peer_for_task, fetch));
        });
        self.drill = DrillState::Loading { peer, bin };
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
            Constraint::Length(3),
            Constraint::Length(20),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let t = theme::active();
        let mut header_l1 = vec![Span::styled(
            "PEERS / TOPOLOGY",
            Style::default().add_modifier(Modifier::BOLD),
        )];
        if let DrillState::Loaded { view } = &self.drill {
            header_l1.push(Span::raw("   · drill "));
            header_l1.push(Span::styled(
                view.peer_overlay.clone(),
                Style::default().fg(t.info),
            ));
        } else if let DrillState::Loading { peer, .. } = &self.drill {
            header_l1.push(Span::raw("   · drill "));
            header_l1.push(Span::styled(peer.clone(), Style::default().fg(t.info)));
            header_l1.push(Span::raw(" (loading)"));
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
        } else if let Some(view) = view_for(&self.snapshot) {
            let s = view.saturation;
            if s.is_alert() {
                let mut spans = vec![
                    Span::styled(
                        format!("  {} STARVING ", t.glyphs.fail),
                        Style::default().fg(t.fail).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{} of {} relevant bins", s.starving, s.relevant)),
                ];
                if let Some(b) = s.worst_bin {
                    spans.push(Span::raw(format!(
                        " · worst bin {b} ({}/{})",
                        s.worst_connected, SATURATION_PEERS
                    )));
                }
                if s.over > 0 {
                    spans.push(Span::styled(
                        format!("  · {} over-saturated", s.over),
                        Style::default().fg(t.warn),
                    ));
                }
                header_l2.extend(spans);
            } else {
                header_l2.push(Span::styled(
                    format!(
                        "  {} all {} relevant bins healthy",
                        t.glyphs.pass, s.relevant
                    ),
                    Style::default().fg(t.pass),
                ));
                if s.over > 0 {
                    header_l2.push(Span::styled(
                        format!(" · {} over-saturated", s.over),
                        Style::default().fg(t.warn),
                    ));
                }
            }
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let view = match view_for(&self.snapshot) {
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

        let mut strip_lines: Vec<Line> = vec![Line::from(vec![Span::styled(
            format!(
                "  depth {} · connected {} / known {} · reachability {} · net {} · blocklisted {}",
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
                view.blocklist.len(),
            ),
            Style::default().fg(t.dim),
        )])];
        // Blocklisted peers (GET /blocklist) — usually empty; list them
        // when present so an operator can see who this node has dropped.
        if !view.blocklist.is_empty() {
            let names: Vec<String> = view
                .blocklist
                .iter()
                .take(6)
                .map(|b| b.peer_short.clone())
                .collect();
            let more = view.blocklist.len().saturating_sub(6);
            let suffix = if more > 0 {
                format!(" +{more} more")
            } else {
                String::new()
            };
            strip_lines.push(Line::from(Span::styled(
                format!("  blocklist: {}{}", names.join(", "), suffix),
                Style::default().fg(t.warn),
            )));
        }
        strip_lines.push(Line::from(Span::styled(
            "  BIN  POP  CONN  BAR              STATUS",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        )));
        for r in &view.bins {
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
                Span::styled(
                    format!("{bar:<14}"),
                    Style::default().fg(bin_color(r.status)),
                ),
                Span::raw(" "),
                Span::styled(
                    bin_label(r.status),
                    Style::default()
                        .fg(bin_color(r.status))
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

        if matches!(self.drill, DrillState::Idle) && !view.peers.is_empty() {
            let i = self.selected.min(view.peers.len() - 1);
            let row = &view.peers[i];
            let detail = Line::from(vec![
                Span::styled("  selected: ", Style::default().fg(t.dim)),
                Span::styled(row.peer_full.clone(), Style::default().fg(t.info)),
                Span::raw("  bin "),
                Span::styled(row.bin.to_string(), Style::default().fg(t.dim)),
            ]);
            frame.render_widget(Paragraph::new(detail), chunks[3]);
        }

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
        frame.render_widget(Paragraph::new(footer), chunks[4]);

        Ok(())
    }
}

impl Peers {
    fn draw_peer_table(&mut self, frame: &mut Frame, area: Rect, peers: &[PeerRow]) {
        let t = theme::active();
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
        lines.push(drill_field_line("storage radius ", &view.storage_radius, t));
        lines.push(drill_field_line("reserve size   ", &view.reserve_size, t));
        lines.push(drill_field_line("pullsync rate  ", &view.pullsync_rate, t));
        lines.push(drill_batch_commitment_line(
            "batch commit   ",
            &view.batch_commitment,
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

fn drill_batch_commitment_line(
    label: &str,
    field: &DrillField<BatchCommitmentCell>,
    t: &theme::Theme,
) -> Line<'static> {
    match field {
        DrillField::Ok(cell) => {
            let value_style = if cell.outlier {
                Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
            } else {
                Style::default().add_modifier(Modifier::BOLD)
            };
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(label.to_string(), Style::default().fg(t.dim)),
                Span::raw("  "),
                Span::styled(cell.formatted.clone(), value_style),
            ];
            if cell.outlier {
                spans.push(Span::styled(
                    "  (>5% off local — outlier)",
                    Style::default().fg(t.fail).add_modifier(Modifier::ITALIC),
                ));
            }
            Line::from(spans)
        }
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

    #[test]
    fn bin_bar_caps_at_oversaturation() {
        let bar_full = bin_bar(50, 12);
        assert_eq!(bar_full, "▇".repeat(12));
        let bar_empty = bin_bar(0, 12);
        assert_eq!(bar_empty, "░".repeat(12));
    }
}
