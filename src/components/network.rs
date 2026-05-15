//! S7 — Network / NAT screen (`docs/PLAN.md` § 8.S7). Pure view-data
//! half lives in [`bee_cockpit_core::views::network`]; this module
//! owns the watch subscriptions, the rolling reachability-stability
//! tracker, the scroll cursor, and the ratatui draw path.

use std::time::Instant;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::network::{
    AvailabilityStatus, NetworkView, ReachabilityStatus, UnderlayKind, UnderlayRow,
    classify_multiaddr, format_stability, view_for,
};

use super::Component;
use crate::action::Action;
use crate::theme;
use crate::watch::{NetworkSnapshot, TopologySnapshot};

fn reachability_color(s: &ReachabilityStatus) -> Color {
    match s {
        ReachabilityStatus::NotLoaded => theme::active().dim,
        ReachabilityStatus::Public => theme::active().pass,
        ReachabilityStatus::Private => theme::active().warn,
        ReachabilityStatus::Other(_) => theme::active().dim,
    }
}

fn availability_color(s: &AvailabilityStatus) -> Color {
    match s {
        AvailabilityStatus::NotLoaded => theme::active().dim,
        AvailabilityStatus::Available => theme::active().pass,
        AvailabilityStatus::Unavailable => theme::active().fail,
        AvailabilityStatus::Other(_) => theme::active().dim,
    }
}

pub struct Network {
    network_rx: watch::Receiver<NetworkSnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    network: NetworkSnapshot,
    topology: TopologySnapshot,
    last_seen_reachability: Option<String>,
    reachability_changed_at: Option<Instant>,
    scroll_offset: usize,
}

impl Network {
    pub fn new(
        network_rx: watch::Receiver<NetworkSnapshot>,
        topology_rx: watch::Receiver<TopologySnapshot>,
    ) -> Self {
        let network = network_rx.borrow().clone();
        let topology = topology_rx.borrow().clone();
        Self {
            network_rx,
            topology_rx,
            network,
            topology,
            last_seen_reachability: None,
            reachability_changed_at: None,
            scroll_offset: 0,
        }
    }

    fn pull_latest(&mut self) {
        self.network = self.network_rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
        let current = self
            .topology
            .topology
            .as_ref()
            .map(|t| t.reachability.clone());
        if current != self.last_seen_reachability {
            self.last_seen_reachability = current;
            self.reachability_changed_at = Some(Instant::now());
        }
    }

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Network::view_for` call sites resolve.
    pub fn view_for(network: &NetworkSnapshot, topology: &TopologySnapshot) -> NetworkView {
        view_for(network, topology)
    }
}

impl Component for Network {
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
            Constraint::Length(4), // identity
            Constraint::Length(4), // connections + reachability
            Constraint::Min(0),    // public addresses
            Constraint::Length(1), // footer
        ])
        .split(area);

        let header_l1 = Line::from(vec![Span::styled(
            "NETWORK / NAT",
            Style::default().add_modifier(Modifier::BOLD),
        )]);
        let mut header_l2 = Vec::new();
        let t = theme::active();
        if let Some(err) = &self.network.last_error {
            let (color, msg) = theme::classify_header_error(err);
            header_l2.push(Span::styled(msg, Style::default().fg(color)));
        } else if !self.network.is_loaded() {
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

        let view = view_for(&self.network, &self.topology);

        let identity = vec![
            Line::from(vec![
                Span::styled(
                    "  overlay   ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(view.overlay_full.clone(), Style::default().fg(t.info)),
            ]),
            Line::from(vec![
                Span::styled(
                    "  ethereum  ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(view.ethereum_full.clone(), Style::default().fg(t.info)),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(identity).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        let stability = self
            .reachability_changed_at
            .map(|tt| format_stability(Instant::now().saturating_duration_since(tt)))
            .unwrap_or_else(|| "—".into());
        let conns = vec![
            Line::from(vec![
                Span::styled(
                    "  inbound   ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(format!("{:<6}", view.inbound)),
                Span::styled("outbound  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::raw(format!("{}", view.outbound)),
            ]),
            Line::from(vec![
                Span::styled(
                    "  reachable ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    view.reachability.label(),
                    Style::default()
                        .fg(reachability_color(&view.reachability))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  (stable for {stability})"),
                    Style::default().fg(t.dim),
                ),
            ]),
            Line::from(vec![
                Span::styled(
                    "  network   ",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    view.network_availability.label(),
                    Style::default()
                        .fg(availability_color(&view.network_availability))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(conns).block(Block::default().borders(Borders::BOTTOM)),
            chunks[2],
        );

        let mut addr_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  PUBLIC ADDRESSES",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if view.underlays.is_empty() {
            addr_lines.push(Line::from(Span::styled(
                "  (no addresses reported)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            for u in &view.underlays {
                let (style, badge) = match u.kind {
                    UnderlayKind::Public => (Style::default().fg(t.pass), " PUB "),
                    UnderlayKind::Private => (Style::default().fg(t.dim), " PRIV"),
                    UnderlayKind::Unknown => (Style::default().fg(t.warn), " ??? "),
                };
                addr_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(format!("[{badge}] "), style),
                    Span::styled(u.multiaddr.clone(), style),
                ]));
            }
            addr_lines.push(Line::from(""));
            addr_lines.push(Line::from(Span::styled(
                "  External port-check + relay candidates require services Bee doesn't expose;",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
            addr_lines.push(Line::from(Span::styled(
                "  use `nmap -p 1634 <ip>` from a separate machine to confirm public reachability.",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        let addr_area = chunks[3];
        let visible_rows = addr_area.height as usize;
        let addr_total = addr_lines.len();
        self.scroll_offset =
            super::scroll::clamp_offset(self.scroll_offset, visible_rows, addr_total);
        frame.render_widget(
            Paragraph::new(addr_lines).scroll((self.scroll_offset as u16, 0)),
            addr_area,
        );
        super::scroll::render_scrollbar(
            frame,
            addr_area,
            self.scroll_offset,
            visible_rows,
            addr_total,
        );

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
                    "isReachable flickers under symmetric NAT — watch the stability window",
                    Style::default().fg(t.dim),
                ),
            ])),
            chunks[4],
        );

        Ok(())
    }
}
