//! Snapshot tests for S7 Network/NAT view computation.
//!
//! [`bee_tui::components::network::Network::view_for`] is the pure
//! function the draw path delegates to (the stability window is
//! tracked in component state and rendered separately because it
//! depends on wall-clock time).
//!
//! Snapshotting it pins the underlay public/private classification,
//! the reachability ladder (`Public` / `Private` / `Other` / unknown),
//! the inbound/outbound peer-direction counts, and the short overlay
//! formatting. Update via `cargo insta review` after intentional copy
//! edits.

use std::time::Instant;

use bee::debug::{Addresses, BinInfo, MetricSnapshotView, PeerInfo, Topology};
use bee_tui::components::network::Network;
use bee_tui::watch::{NetworkSnapshot, TopologySnapshot};

fn addresses(overlay_byte: u8, eth_byte: u8, underlays: Vec<&str>) -> Addresses {
    Addresses {
        overlay: format!("{:02x}", overlay_byte).repeat(32),
        underlay: underlays.into_iter().map(String::from).collect(),
        ethereum: format!("0x{}", format!("{:02x}", eth_byte).repeat(20)),
        public_key: "00".repeat(33),
        pss_public_key: "00".repeat(33),
    }
}

fn topology(reachability: &str, network_availability: &str, peers: Vec<PeerInfo>) -> Topology {
    let mut bins = vec![BinInfo::default(); 32];
    bins[4] = BinInfo {
        population: peers.len() as u64,
        connected: peers.len() as u64,
        connected_peers: peers,
        disconnected_peers: vec![],
    };
    Topology {
        base_addr: "ab".repeat(32),
        population: 0,
        connected: 0,
        timestamp: "2024-01-01T00:00:00Z".into(),
        nn_low_watermark: 8,
        depth: 8,
        reachability: reachability.into(),
        network_availability: network_availability.into(),
        bins,
        light_nodes: BinInfo::default(),
    }
}

fn peer_with_direction(addr_byte: u8, direction: &str) -> PeerInfo {
    PeerInfo {
        address: format!("{:02x}", addr_byte).repeat(32),
        metrics: Some(MetricSnapshotView {
            session_connection_direction: direction.into(),
            ..MetricSnapshotView::default()
        }),
    }
}

fn snapshot_with(
    addresses: Option<Addresses>,
    topology: Option<Topology>,
) -> (NetworkSnapshot, TopologySnapshot) {
    (
        NetworkSnapshot {
            addresses,
            last_error: None,
            last_update: Some(Instant::now()),
        },
        TopologySnapshot {
            topology,
            last_error: None,
            last_update: Some(Instant::now()),
        },
    )
}

#[test]
fn view_empty_snapshots() {
    let (n, t) = snapshot_with(None, None);
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_public_node() {
    // Operator-friendly setup: public IPv4 + IPv6, healthy AutoNAT,
    // mostly inbound peers (the node is acting as a server).
    let addrs = addresses(
        0xaa,
        0xbb,
        vec![
            "/ip4/77.123.45.6/tcp/1634/p2p/16Uiu...",
            "/ip6/2a01:4f8:1:2::1/tcp/1634/p2p/16Uiu...",
        ],
    );
    let topo = topology(
        "Public",
        "Available",
        vec![
            peer_with_direction(0x01, "inbound"),
            peer_with_direction(0x02, "inbound"),
            peer_with_direction(0x03, "inbound"),
            peer_with_direction(0x04, "outbound"),
        ],
    );
    let (n, t) = snapshot_with(Some(addrs), Some(topo));
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_private_node_behind_nat() {
    // Symptomatic: only RFC1918 + loopback addresses surfaced;
    // AutoNAT reports Private and the node is making mostly outbound
    // connections (no one can reach in).
    let addrs = addresses(
        0xcc,
        0xdd,
        vec![
            "/ip4/127.0.0.1/tcp/1634/p2p/16Uiu...",
            "/ip4/192.168.1.5/tcp/1634/p2p/16Uiu...",
            "/ip4/10.0.0.5/tcp/1634/p2p/16Uiu...",
        ],
    );
    let topo = topology(
        "Private",
        "Available",
        vec![
            peer_with_direction(0x01, "outbound"),
            peer_with_direction(0x02, "outbound"),
            peer_with_direction(0x03, "outbound"),
            peer_with_direction(0x04, "outbound"),
        ],
    );
    let (n, t) = snapshot_with(Some(addrs), Some(topo));
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_dns_multiaddr_classified_as_unknown() {
    // DNS multiaddrs can't be classified without resolving — they
    // surface as Unknown so the operator knows the screen can't
    // tell.
    let addrs = addresses(
        0xee,
        0xff,
        vec!["/dns4/bee.example.com/tcp/1634/p2p/16Uiu..."],
    );
    let topo = topology("Public", "Available", vec![]);
    let (n, t) = snapshot_with(Some(addrs), Some(topo));
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_unknown_reachability_string_passes_through() {
    // Defensive: future Bee builds may add new reachability values.
    // We surface them verbatim instead of misclassifying.
    let topo = topology("Symmetric", "Unavailable", vec![]);
    let (n, t) = snapshot_with(
        Some(addresses(0x11, 0x22, vec![])),
        Some(topo),
    );
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_topology_loaded_but_addresses_missing() {
    // /topology arrives first (5 s) before /addresses (60 s) on
    // first poll. View should still render reachability + counts,
    // just no address rows.
    let topo = topology(
        "Public",
        "Available",
        vec![peer_with_direction(0x01, "inbound")],
    );
    let (n, t) = snapshot_with(None, Some(topo));
    let view = Network::view_for(&n, &t);
    insta::assert_debug_snapshot!(view);
}
