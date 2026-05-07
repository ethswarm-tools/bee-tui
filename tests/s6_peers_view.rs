//! Snapshot tests for S6 Peers view computation.
//!
//! [`bee_tui::components::peers::Peers::view_for`] is the pure
//! function the draw path delegates to. Snapshotting it pins down
//! the bin-saturation ladder, the depth-relative "is_relevant"
//! gating that prevents far-empty bins from spamming Starving, and
//! the peer-row formatting (overlay shortening, latency unit
//! conversion, direction normalization). Update via
//! `cargo insta review` after intentional copy edits.

use std::time::Instant;

use bee::debug::{BinInfo, MetricSnapshotView, PeerInfo, Topology};
use bee_tui::components::peers::{
    OVER_SATURATION_PEERS, Peers, SATURATION_PEERS,
};
use bee_tui::watch::TopologySnapshot;

fn empty_bin() -> BinInfo {
    BinInfo::default()
}

fn bin_with_peers(population: u64, connected: u64, peers: Vec<PeerInfo>) -> BinInfo {
    BinInfo {
        population,
        connected,
        connected_peers: peers,
        disconnected_peers: Vec::new(),
    }
}

fn peer(addr_byte: u8, dir: &str, latency_ns: i64, healthy: bool, reachability: &str) -> PeerInfo {
    let addr = format!("{:02x}", addr_byte).repeat(32);
    PeerInfo {
        address: addr,
        metrics: Some(MetricSnapshotView {
            session_connection_direction: dir.into(),
            latency_ewma: latency_ns,
            healthy,
            reachability: reachability.into(),
            ..MetricSnapshotView::default()
        }),
    }
}

fn peer_no_metrics(addr_byte: u8) -> PeerInfo {
    let addr = format!("{:02x}", addr_byte).repeat(32);
    PeerInfo {
        address: addr,
        metrics: None,
    }
}

fn topology_with(depth: u8, bins: Vec<BinInfo>) -> Topology {
    // Pad to 32 if caller gave fewer.
    let mut full = bins;
    while full.len() < 32 {
        full.push(empty_bin());
    }
    Topology {
        base_addr: "ab".repeat(32),
        population: full
            .iter()
            .map(|b| b.population as i64)
            .sum::<i64>(),
        connected: full
            .iter()
            .map(|b| b.connected as i64)
            .sum::<i64>(),
        timestamp: "2024-01-01T00:00:00Z".into(),
        nn_low_watermark: SATURATION_PEERS as i64,
        depth,
        reachability: "Public".into(),
        network_availability: "Available".into(),
        bins: full,
        light_nodes: BinInfo::default(),
    }
}

fn snapshot_with(topology: Topology) -> TopologySnapshot {
    TopologySnapshot {
        topology: Some(topology),
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_no_topology_yet() {
    let view = Peers::view_for(&TopologySnapshot::default());
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_empty_node() {
    // Brand-new node: depth 0, no peers anywhere. Every bin should
    // classify as Starving (since bin <= depth + 4 = 4 marks them as
    // relevant) or Empty (far bins).
    let t = topology_with(0, vec![]);
    let view = Peers::view_for(&snapshot_with(t)).unwrap();
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_healthy_node_at_depth_8() {
    // Node has saturated bins 0..=8 with 12 connected peers each (in
    // Healthy band) and a few peers in further bins. Bins beyond
    // depth + 4 (=12) should classify as Empty, not Starving.
    let mut bins = Vec::new();
    for _ in 0..=8 {
        bins.push(bin_with_peers(15, 12, vec![]));
    }
    // Bin 9..=11 with low population — relevant (within depth+4),
    // should be Starving.
    bins.push(bin_with_peers(3, 2, vec![]));
    bins.push(bin_with_peers(1, 1, vec![]));
    bins.push(bin_with_peers(0, 0, vec![]));
    // Bins 12+ remain empty (default) — far, should be Empty not Starving.
    let t = topology_with(8, bins);
    let view = Peers::view_for(&snapshot_with(t)).unwrap();
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_oversaturated_bin() {
    // Bin 4 has 25 connected — over the OVER_SATURATION_PEERS=18 threshold.
    let mut bins = Vec::new();
    for i in 0..=8u8 {
        let connected = if i == 4 { OVER_SATURATION_PEERS + 7 } else { 12 };
        bins.push(bin_with_peers(connected + 5, connected, vec![]));
    }
    let t = topology_with(8, bins);
    let view = Peers::view_for(&snapshot_with(t)).unwrap();
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_peer_table_with_metrics_and_without() {
    // Mix: bin 4 has three peers — two with metrics (one healthy,
    // one not), one with no metrics at all. Tests latency formatting
    // (ns → ms), direction normalization, and the no-metrics
    // fallback.
    let mut bins = Vec::new();
    for _ in 0..=3u8 {
        bins.push(bin_with_peers(10, 9, vec![]));
    }
    bins.push(bin_with_peers(
        4,
        3,
        vec![
            // Healthy outbound peer with 8.4 ms latency.
            peer(0xaa, "outbound", 8_400_000, true, "Public"),
            // Unhealthy inbound peer with 240 ms latency.
            peer(0xbb, "inbound", 240_000_000, false, "Private"),
            // No metrics at all — defaults to "?" / "—".
            peer_no_metrics(0xcc),
        ],
    ));
    // Pad to depth+1.
    for _ in 5..=8u8 {
        bins.push(bin_with_peers(10, 9, vec![]));
    }
    let t = topology_with(8, bins);
    let view = Peers::view_for(&snapshot_with(t)).unwrap();
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_peer_rows_sort_stably() {
    // Multiple peers across multiple bins — verify the output is
    // sorted by (bin asc, overlay asc) so the table doesn't shuffle
    // every poll tick.
    let mut bins = vec![empty_bin(); 32];
    bins[2] = bin_with_peers(
        2,
        2,
        vec![
            peer(0xff, "outbound", 1_000_000, true, "Public"),
            peer(0x10, "outbound", 1_000_000, true, "Public"),
        ],
    );
    bins[5] = bin_with_peers(
        2,
        2,
        vec![
            peer(0x80, "inbound", 1_000_000, true, "Public"),
            peer(0x70, "inbound", 1_000_000, true, "Public"),
        ],
    );
    let t = topology_with(8, bins);
    let view = Peers::view_for(&snapshot_with(t)).unwrap();
    insta::assert_debug_snapshot!(view);
}
