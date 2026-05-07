//! S7 — Network / NAT screen (`docs/PLAN.md` § 8.S7).
//!
//! Answers "I have peers but I'm unreachable" (bee#4194) by surfacing
//! what the AutoNAT subsystem and `/addresses` already report:
//!
//! - **Public addresses** advertised by the node — every underlay
//!   multiaddr from `/addresses`. Loopback and private RFC1918
//!   ranges are dimmed so it's obvious which entries are routable
//!   from outside.
//! - **Inbound vs outbound** connection counts derived from each
//!   peer's `MetricSnapshotView::session_connection_direction`.
//!   This is the numbers operators want to see — a node that's
//!   only making outbound connections is reachable in name only.
//! - **Reachability + network availability** read off the
//!   `/topology` stream's AutoNAT strings, with a *stability
//!   window* ("stable for 9m") computed in the component. The
//!   tooltip captures the truth that `isReachable` flickers under
//!   symmetric NAT, so operators don't chase phantom flap.
//!
//! The truth-telling extras from PLAN (external port-check, relay
//! candidates) require either a separate observer service or fields
//! Bee doesn't expose today; the screen footer documents that gap.
//!
//! Render path delegates to the pure [`Network::view_for`] so the
//! snapshot tests in `tests/s7_network_view.rs` pin the
//! reachability ladder and the public/private underlay
//! classification without launching a TUI.

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
use crate::watch::{NetworkSnapshot, TopologySnapshot};

use bee::debug::{Addresses, Topology};

/// Tri-state for the AutoNAT-reported reachability string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReachabilityStatus {
    /// Bee has not reported a reachability string yet (older builds
    /// or topology not loaded).
    NotLoaded,
    /// AutoNAT says the node is publicly reachable.
    Public,
    /// AutoNAT says the node is behind NAT and not directly reachable.
    Private,
    /// Some other string Bee surfaced — kept verbatim so we don't
    /// silently misclassify future API additions.
    Other(String),
}

impl ReachabilityStatus {
    fn from_api(s: &str) -> Self {
        match s {
            "" => Self::NotLoaded,
            "Public" => Self::Public,
            "Private" => Self::Private,
            other => Self::Other(other.into()),
        }
    }
    fn color(&self) -> Color {
        match self {
            Self::NotLoaded => theme::active().dim,
            Self::Public => theme::active().pass,
            Self::Private => theme::active().warn,
            Self::Other(_) => theme::active().dim,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::NotLoaded => "(unknown)".into(),
            Self::Public => "Public".into(),
            Self::Private => "Private".into(),
            Self::Other(s) => s.clone(),
        }
    }
}

/// Tri-state for `networkAvailability`. Less ambiguous than
/// reachability — Bee only reports `Available` / `Unavailable` /
/// empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AvailabilityStatus {
    NotLoaded,
    Available,
    Unavailable,
    Other(String),
}

impl AvailabilityStatus {
    fn from_api(s: &str) -> Self {
        match s {
            "" => Self::NotLoaded,
            "Available" => Self::Available,
            "Unavailable" => Self::Unavailable,
            other => Self::Other(other.into()),
        }
    }
    fn color(&self) -> Color {
        match self {
            Self::NotLoaded => theme::active().dim,
            Self::Available => theme::active().pass,
            Self::Unavailable => theme::active().fail,
            Self::Other(_) => theme::active().dim,
        }
    }
    fn label(&self) -> String {
        match self {
            Self::NotLoaded => "(unknown)".into(),
            Self::Available => "Available".into(),
            Self::Unavailable => "Unavailable".into(),
            Self::Other(s) => s.clone(),
        }
    }
}

/// Per-underlay public/private classification — used to dim non-
/// routable rows so operators see which addresses are actually
/// advertised to the network.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnderlayKind {
    /// Routable IPv4/IPv6 — what the node advertises as a peer-dial
    /// address.
    Public,
    /// RFC 1918 / link-local / loopback — visible inside the host
    /// but not directly dialable from the open internet.
    Private,
    /// Couldn't classify (hostname, DNS multiaddr, exotic transport).
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnderlayRow {
    pub multiaddr: String,
    pub kind: UnderlayKind,
}

/// Aggregated view fed to renderer and snapshot tests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkView {
    pub overlay_short: String,
    pub ethereum_short: String,
    pub underlays: Vec<UnderlayRow>,
    pub inbound: u64,
    pub outbound: u64,
    pub reachability: ReachabilityStatus,
    pub network_availability: AvailabilityStatus,
}

pub struct Network {
    network_rx: watch::Receiver<NetworkSnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    network: NetworkSnapshot,
    topology: TopologySnapshot,
    /// Stability tracking. `last_seen_reachability` is the value we
    /// most recently observed; `reachability_changed_at` is when we
    /// first saw it. `Instant::now() - reachability_changed_at` is
    /// the "stable for X" window the tooltip displays.
    last_seen_reachability: Option<String>,
    reachability_changed_at: Option<Instant>,
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
        }
    }

    fn pull_latest(&mut self) {
        self.network = self.network_rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
        // Stability bookkeeping: if the reachability string changed
        // since last tick, reset the changed-at timestamp.
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

    /// Pure, snapshot-driven view computation. Exposed for snapshot
    /// tests.
    pub fn view_for(network: &NetworkSnapshot, topology: &TopologySnapshot) -> NetworkView {
        let addresses = network.addresses.as_ref();
        let topo = topology.topology.as_ref();
        let underlays = addresses.map(underlay_rows).unwrap_or_default();
        let (inbound, outbound) = topo.map(peer_direction_counts).unwrap_or((0, 0));
        NetworkView {
            overlay_short: addresses
                .map(|a| short_hex(&a.overlay))
                .unwrap_or_else(|| "—".into()),
            ethereum_short: addresses
                .map(|a| short_hex(&a.ethereum))
                .unwrap_or_else(|| "—".into()),
            underlays,
            inbound,
            outbound,
            reachability: topo
                .map(|t| ReachabilityStatus::from_api(&t.reachability))
                .unwrap_or(ReachabilityStatus::NotLoaded),
            network_availability: topo
                .map(|t| AvailabilityStatus::from_api(&t.network_availability))
                .unwrap_or(AvailabilityStatus::NotLoaded),
        }
    }
}

fn underlay_rows(a: &Addresses) -> Vec<UnderlayRow> {
    a.underlay
        .iter()
        .map(|m| UnderlayRow {
            multiaddr: m.clone(),
            kind: classify_multiaddr(m),
        })
        .collect()
}

/// Classify a multiaddr as Public / Private / Unknown by parsing the
/// `/ip4/x.x.x.x` or `/ip6/...` segment. RFC 1918, link-local, and
/// loopback ranges count as Private; DNS/hostname multiaddrs we leave
/// as Unknown because we can't tell without resolving.
pub fn classify_multiaddr(m: &str) -> UnderlayKind {
    let parts: Vec<&str> = m.split('/').collect();
    // Look for /ip4/x.x.x.x or /ip6/x... in the protocol stack.
    let mut i = 0;
    while i + 1 < parts.len() {
        match parts[i] {
            "ip4" => return classify_ipv4(parts[i + 1]),
            "ip6" => return classify_ipv6(parts[i + 1]),
            "dns" | "dns4" | "dns6" | "dnsaddr" => return UnderlayKind::Unknown,
            _ => {}
        }
        i += 1;
    }
    UnderlayKind::Unknown
}

fn classify_ipv4(addr: &str) -> UnderlayKind {
    let octets: Vec<u8> = match addr
        .split('.')
        .map(|s| s.parse::<u8>())
        .collect::<Result<Vec<_>, _>>()
    {
        Ok(o) if o.len() == 4 => o,
        _ => return UnderlayKind::Unknown,
    };
    // RFC 1918, loopback, link-local, CGNAT.
    let is_private = matches!(octets[0], 10)
        || (octets[0] == 172 && (16..=31).contains(&octets[1]))
        || (octets[0] == 192 && octets[1] == 168)
        || octets[0] == 127
        || (octets[0] == 169 && octets[1] == 254)
        || (octets[0] == 100 && (64..=127).contains(&octets[1]))
        || octets[0] == 0;
    if is_private {
        UnderlayKind::Private
    } else {
        UnderlayKind::Public
    }
}

fn classify_ipv6(addr: &str) -> UnderlayKind {
    let lower = addr.to_lowercase();
    // Loopback ::1.
    if lower == "::1" {
        return UnderlayKind::Private;
    }
    // Unique-local fc00::/7 (any first hextet starting with fc/fd).
    if lower.starts_with("fc") || lower.starts_with("fd") {
        return UnderlayKind::Private;
    }
    // Link-local fe80::/10.
    if lower.starts_with("fe8")
        || lower.starts_with("fe9")
        || lower.starts_with("fea")
        || lower.starts_with("feb")
    {
        return UnderlayKind::Private;
    }
    UnderlayKind::Public
}

fn peer_direction_counts(t: &Topology) -> (u64, u64) {
    let mut inbound = 0u64;
    let mut outbound = 0u64;
    for b in &t.bins {
        for p in &b.connected_peers {
            if let Some(m) = &p.metrics {
                match m.session_connection_direction.as_str() {
                    "inbound" => inbound += 1,
                    "outbound" => outbound += 1,
                    _ => {}
                }
            }
        }
    }
    (inbound, outbound)
}

fn short_hex(s: &str) -> String {
    let trimmed = s.trim_start_matches("0x");
    if trimmed.len() > 12 {
        format!("{}…{}", &trimmed[..6], &trimmed[trimmed.len() - 4..])
    } else {
        trimmed.to_string()
    }
}

/// Format a stability duration as `Xh Ym`, `Xm Ys`, or `Xs`. Used by
/// the renderer; not part of [`NetworkView`] because it depends on
/// wall-clock state the snapshot tests can't pin.
pub fn format_stability(d: Duration) -> String {
    let secs = d.as_secs();
    if secs >= 3_600 {
        let h = secs / 3_600;
        let m = (secs % 3_600) / 60;
        format!("{h}h {m:>2}m")
    } else if secs >= 60 {
        let m = secs / 60;
        let s = secs % 60;
        format!("{m}m {s:>2}s")
    } else {
        format!("{secs}s")
    }
}

impl Component for Network {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
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

        // Header
        let header_l1 = Line::from(vec![Span::styled(
            "NETWORK / NAT",
            Style::default().add_modifier(Modifier::BOLD),
        )]);
        let mut header_l2 = Vec::new();
        let t = theme::active();
        if let Some(err) = &self.network.last_error {
            header_l2.push(Span::styled(
                format!("addresses error: {err}"),
                Style::default().fg(t.fail),
            ));
        } else if !self.network.is_loaded() {
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

        let view = Self::view_for(&self.network, &self.topology);

        // Identity: overlay + ethereum
        let identity = vec![
            Line::from(vec![
                Span::styled("  overlay   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(view.overlay_short.clone(), Style::default().fg(t.info)),
            ]),
            Line::from(vec![
                Span::styled("  ethereum  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    view.ethereum_short.clone(),
                    Style::default().fg(t.info),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(identity).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        // Connections + reachability
        let stability = self
            .reachability_changed_at
            .map(|t| format_stability(Instant::now().saturating_duration_since(t)))
            .unwrap_or_else(|| "—".into());
        let conns = vec![
            Line::from(vec![
                Span::styled("  inbound   ", Style::default().add_modifier(Modifier::BOLD)),
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
                        .fg(view.reachability.color())
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("  (stable for {stability})"),
                    Style::default().fg(t.dim),
                ),
            ]),
            Line::from(vec![
                Span::styled("  network   ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    view.network_availability.label(),
                    Style::default()
                        .fg(view.network_availability.color())
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ];
        frame.render_widget(
            Paragraph::new(conns).block(Block::default().borders(Borders::BOTTOM)),
            chunks[2],
        );

        // Public addresses
        let mut addr_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  PUBLIC ADDRESSES",
            Style::default()
                .fg(t.dim)
                .add_modifier(Modifier::BOLD),
        ))];
        if view.underlays.is_empty() {
            addr_lines.push(Line::from(Span::styled(
                "  (no addresses reported)",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::ITALIC),
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
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
            addr_lines.push(Line::from(Span::styled(
                "  use `nmap -p 1634 <ip>` from a separate machine to confirm public reachability.",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        frame.render_widget(Paragraph::new(addr_lines), chunks[3]);

        // Footer
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_ipv4_public_and_private() {
        assert_eq!(classify_ipv4("8.8.8.8"), UnderlayKind::Public);
        assert_eq!(classify_ipv4("77.123.45.6"), UnderlayKind::Public);
        assert_eq!(classify_ipv4("10.0.0.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("172.16.0.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("172.31.0.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("192.168.1.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("127.0.0.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("169.254.1.1"), UnderlayKind::Private);
        assert_eq!(classify_ipv4("100.64.0.1"), UnderlayKind::Private);
    }

    #[test]
    fn classify_ipv4_invalid_is_unknown() {
        assert_eq!(classify_ipv4("not-an-ip"), UnderlayKind::Unknown);
        assert_eq!(classify_ipv4("1.2.3"), UnderlayKind::Unknown);
    }

    #[test]
    fn classify_ipv6_public_and_private() {
        assert_eq!(classify_ipv6("2a01:4f8:1:2::1"), UnderlayKind::Public);
        assert_eq!(classify_ipv6("::1"), UnderlayKind::Private);
        assert_eq!(classify_ipv6("fc00::1"), UnderlayKind::Private);
        assert_eq!(classify_ipv6("fd12::1"), UnderlayKind::Private);
        assert_eq!(classify_ipv6("fe80::1"), UnderlayKind::Private);
    }

    #[test]
    fn classify_multiaddr_picks_protocol() {
        assert_eq!(
            classify_multiaddr("/ip4/77.123.45.6/tcp/1634/p2p/16Uiu..."),
            UnderlayKind::Public
        );
        assert_eq!(
            classify_multiaddr("/ip4/192.168.1.5/tcp/1634/p2p/16Uiu..."),
            UnderlayKind::Private
        );
        assert_eq!(
            classify_multiaddr("/dns4/bee.example.com/tcp/1634"),
            UnderlayKind::Unknown
        );
        // No /ip4 or /ip6 segment at all.
        assert_eq!(classify_multiaddr("/p2p/16Uiu..."), UnderlayKind::Unknown);
    }

    #[test]
    fn reachability_from_api_known_strings() {
        assert_eq!(ReachabilityStatus::from_api("Public"), ReachabilityStatus::Public);
        assert_eq!(
            ReachabilityStatus::from_api("Private"),
            ReachabilityStatus::Private
        );
        assert_eq!(
            ReachabilityStatus::from_api(""),
            ReachabilityStatus::NotLoaded
        );
        assert_eq!(
            ReachabilityStatus::from_api("Symmetric"),
            ReachabilityStatus::Other("Symmetric".into())
        );
    }

    #[test]
    fn format_stability_unit_thresholds() {
        assert_eq!(format_stability(Duration::from_secs(5)), "5s");
        assert_eq!(format_stability(Duration::from_secs(125)), "2m  5s");
        assert_eq!(format_stability(Duration::from_secs(3_725)), "1h  2m");
    }
}
