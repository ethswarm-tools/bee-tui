//! Fleet view — the v1.11 "look at all my nodes at once" surface.
//!
//! Where the rest of bee-tui builds *one* `BeeWatch` hub against the
//! active `[[nodes]]` entry, the fleet poller fans a *cheap* health
//! probe out to **every** configured node in parallel and aggregates
//! the results into one snapshot the S15 Fleet screen renders. Three
//! endpoints per node (`/health`, `/status`, `/stamps`) — no
//! `/topology`, `/wallet`, `/chainstate`, `/redistributionstate` —
//! keeps the fan-out cheap (~3 reqs × N nodes / 10 s).
//!
//! Dead / slow nodes time out (5 s per probe) without blocking the
//! others; the row goes `Fail` with an error string until the node
//! recovers. The poller never panics or breaks the loop — every error
//! becomes a row status.

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures::stream::{FuturesUnordered, StreamExt};
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::api::ApiClient;
use crate::config::NodeConfig;

/// Per-probe timeout. A node that doesn't respond within this window
/// is marked `Fail` for the current cycle but stays in the rotation.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Minimum reasonable "connected peers" count before the row goes
/// Warn. Matches the rule of thumb in operator threads — a node with
/// fewer than 4 peers is basically isolated.
pub const PEERS_WARN_THRESHOLD: u64 = 4;

/// Aggregate status of one node row. Worst-of: API reachability,
/// warmup state, peer count, stamp TTL. The S15 view sorts so all
/// `Fail` rows are visible without scrolling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FleetStatus {
    /// Never successfully probed (e.g. cold start, first 10 s).
    #[default]
    Unknown,
    /// All checks pass.
    Pass,
    /// At least one check is in its warn band; nothing critical.
    Warn,
    /// API unreachable, auth failed, or a check tripped its critical
    /// threshold. Worth a webhook ping on the underlying single-node
    /// alerter.
    Fail,
}

impl FleetStatus {
    /// Worst-of two statuses; the partial-order is
    /// `Pass < Warn < Fail < Unknown`. (Unknown is treated as the
    /// "worst" so a half-probed node renders Unknown until every
    /// check has a real answer — avoids a misleading green row when
    /// the second endpoint hasn't returned yet.)
    pub fn worst(self, other: FleetStatus) -> FleetStatus {
        use FleetStatus::*;
        match (self, other) {
            (Unknown, _) | (_, Unknown) => Unknown,
            (Fail, _) | (_, Fail) => Fail,
            (Warn, _) | (_, Warn) => Warn,
            _ => Pass,
        }
    }
}

/// One row of the fleet view. Built fresh on every probe cycle.
#[derive(Debug, Clone)]
pub struct FleetRow {
    pub name: String,
    pub url: String,
    pub default: bool,
    pub status: FleetStatus,
    /// `None` while the row is `Unknown` or the probe failed before
    /// /status returned.
    pub peers: Option<u64>,
    /// `Some(seconds)` when at least one usable batch was reported;
    /// `None` when no usable batches or probe failed.
    pub worst_ttl_secs: Option<u64>,
    /// `/health` round-trip in milliseconds. `None` if unreachable.
    pub ping_ms: Option<u64>,
    /// `true` when `/status` says `is_warming_up` is true. Drives a
    /// dedicated `warming` status in the table.
    pub warming_up: bool,
    /// When the row was last probed. `None` on the initial
    /// `FleetSnapshot::default()` only.
    pub last_probe: Option<Instant>,
    /// One-line operator-facing reason this row is in its current
    /// state. Set only when status is `Warn` or `Fail`.
    pub why: Option<String>,
}

impl FleetRow {
    fn unknown(name: String, url: String, default: bool) -> Self {
        Self {
            name,
            url,
            default,
            status: FleetStatus::Unknown,
            peers: None,
            worst_ttl_secs: None,
            ping_ms: None,
            warming_up: false,
            last_probe: None,
            why: None,
        }
    }
}

/// The whole-fleet snapshot. The S15 screen receives this via
/// `watch::Receiver` and renders rows in the same order as
/// `config.nodes` (stable across cycles so cursor position doesn't
/// jump).
#[derive(Debug, Clone, Default)]
pub struct FleetSnapshot {
    pub rows: Vec<FleetRow>,
    pub last_update: Option<Instant>,
}

impl FleetSnapshot {
    /// Initial snapshot before the first probe — one `Unknown` row
    /// per configured node so the screen renders something
    /// reasonable on cold start.
    pub fn seed(nodes: &[NodeConfig]) -> Self {
        Self {
            rows: nodes
                .iter()
                .map(|n| FleetRow::unknown(n.name.clone(), n.url.clone(), n.default))
                .collect(),
            last_update: None,
        }
    }

    /// Roll-up counters for the header line. Returns
    /// `(pass, warn, fail, unknown)`.
    pub fn counts(&self) -> (usize, usize, usize, usize) {
        let (mut p, mut w, mut f, mut u) = (0, 0, 0, 0);
        for r in &self.rows {
            match r.status {
                FleetStatus::Pass => p += 1,
                FleetStatus::Warn => w += 1,
                FleetStatus::Fail => f += 1,
                FleetStatus::Unknown => u += 1,
            }
        }
        (p, w, f, u)
    }
}

/// Spawn the fleet poller as a child of `cancel`. Returns the
/// `watch::Receiver<FleetSnapshot>` the S15 screen subscribes to and
/// the resync-trigger handle the screen uses for the `r` key.
pub fn spawn_poller(
    nodes: Vec<NodeConfig>,
    cancel: CancellationToken,
    interval: Duration,
) -> (
    watch::Receiver<FleetSnapshot>,
    tokio::sync::mpsc::UnboundedSender<()>,
) {
    let (tx, rx) = watch::channel(FleetSnapshot::seed(&nodes));
    let (resync_tx, mut resync_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        // First fire happens immediately so the screen populates on
        // open rather than after the first `interval` wait.
        tick.tick().await;
        loop {
            run_cycle(&nodes, &tx).await;
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => continue,
                _ = resync_rx.recv() => continue, // operator-triggered
            }
        }
    });
    (rx, resync_tx)
}

async fn run_cycle(nodes: &[NodeConfig], tx: &watch::Sender<FleetSnapshot>) {
    let mut futs = FuturesUnordered::new();
    for node in nodes {
        futs.push(probe_node(node.clone()));
    }
    let mut rows_by_name: std::collections::HashMap<String, FleetRow> =
        std::collections::HashMap::with_capacity(nodes.len());
    while let Some(row) = futs.next().await {
        rows_by_name.insert(row.name.clone(), row);
    }
    // Re-order by config index so the table column order stays
    // predictable across cycles (FuturesUnordered yields in
    // completion order; we don't want the table reshuffling).
    let mut rows: Vec<FleetRow> = nodes
        .iter()
        .map(|n| {
            rows_by_name
                .remove(&n.name)
                .unwrap_or_else(|| FleetRow::unknown(n.name.clone(), n.url.clone(), n.default))
        })
        .collect();
    rows.iter_mut().for_each(|r| {
        if r.last_probe.is_none() {
            r.last_probe = Some(Instant::now());
        }
    });
    let _ = tx.send(FleetSnapshot {
        rows,
        last_update: Some(Instant::now()),
    });
}

async fn probe_node(node: NodeConfig) -> FleetRow {
    let name = node.name.clone();
    let url = node.url.clone();
    let default = node.default;
    let api = match ApiClient::from_node(&node) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            return FleetRow {
                name,
                url,
                default,
                status: FleetStatus::Fail,
                peers: None,
                worst_ttl_secs: None,
                ping_ms: None,
                warming_up: false,
                last_probe: Some(Instant::now()),
                why: Some(format!("config: {e}")),
            };
        }
    };

    let probe_started = Instant::now();
    let bee = api.bee();
    let result = tokio::time::timeout(PROBE_TIMEOUT, async {
        // Sequential ping → status → stamps. We could fan out via
        // join! but that triples concurrent in-flight requests
        // against a single node; for a fleet probe we'd rather be
        // gentle.
        let health_ok = bee.debug().health().await.is_ok();
        let status = if health_ok {
            bee.debug().status().await.ok()
        } else {
            None
        };
        let stamps = if health_ok {
            bee.postage().get_postage_batches().await.ok()
        } else {
            None
        };
        (health_ok, status, stamps)
    })
    .await;

    let (health_ok, status, stamps) = match result {
        Ok(t) => t,
        Err(_) => {
            return FleetRow {
                name,
                url,
                default,
                status: FleetStatus::Fail,
                peers: None,
                worst_ttl_secs: None,
                ping_ms: None,
                warming_up: false,
                last_probe: Some(Instant::now()),
                why: Some(format!("probe timed out after {}s", PROBE_TIMEOUT.as_secs())),
            };
        }
    };

    if !health_ok {
        return FleetRow {
            name,
            url,
            default,
            status: FleetStatus::Fail,
            peers: None,
            worst_ttl_secs: None,
            ping_ms: None,
            warming_up: false,
            last_probe: Some(Instant::now()),
            why: Some("unreachable (/health failed)".into()),
        };
    }
    let ping_ms = probe_started.elapsed().as_millis() as u64;

    let warming_up = status.as_ref().map(|s| s.is_warming_up).unwrap_or(false);
    let peers = status.as_ref().map(|s| s.connected_peers.max(0) as u64);

    let worst_ttl_secs = stamps.as_ref().and_then(|batches| {
        batches
            .iter()
            .filter(|b| b.usable)
            .map(|b| b.batch_ttl)
            .min()
            .and_then(|t| if t >= 0 { Some(t as u64) } else { None })
    });

    aggregate(name, url, default, ping_ms, warming_up, peers, worst_ttl_secs)
}

/// Pure: take the probed numbers, return a `FleetRow` with the
/// aggregate status + why-line. Extracted from `probe_node` so tests
/// can assert the status ladder without spinning up an HTTP server.
pub fn aggregate(
    name: String,
    url: String,
    default: bool,
    ping_ms: u64,
    warming_up: bool,
    peers: Option<u64>,
    worst_ttl_secs: Option<u64>,
) -> FleetRow {
    use crate::components::stamps::{TOPUP_SOON_SECS, TOPUP_URGENT_SECS};

    let mut status = FleetStatus::Pass;
    let mut why: Option<String> = None;

    if warming_up {
        status = status.worst(FleetStatus::Warn);
        why = Some("warming up — not yet serving traffic".into());
    }
    if let Some(p) = peers {
        if p == 0 {
            status = status.worst(FleetStatus::Fail);
            why = Some("0 peers — isolated".into());
        } else if p < PEERS_WARN_THRESHOLD {
            status = status.worst(FleetStatus::Warn);
            if why.is_none() {
                why = Some(format!("only {p} peers (< {PEERS_WARN_THRESHOLD})"));
            }
        }
    }
    if let Some(ttl) = worst_ttl_secs {
        let ttl_i = ttl as i64;
        if ttl_i <= TOPUP_URGENT_SECS {
            status = status.worst(FleetStatus::Fail);
            why = Some(format!(
                "stamp TTL under {}h — topup URGENT",
                TOPUP_URGENT_SECS / 3600
            ));
        } else if ttl_i <= TOPUP_SOON_SECS {
            status = status.worst(FleetStatus::Warn);
            if why.is_none() {
                why = Some(format!(
                    "stamp TTL under {}d — plan a topup",
                    TOPUP_SOON_SECS / 86_400
                ));
            }
        }
    }

    FleetRow {
        name,
        url,
        default,
        status,
        peers,
        worst_ttl_secs,
        ping_ms: Some(ping_ms),
        warming_up,
        last_probe: Some(Instant::now()),
        why,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn worst_of_picks_the_worse_of_two() {
        assert_eq!(
            FleetStatus::Pass.worst(FleetStatus::Warn),
            FleetStatus::Warn
        );
        assert_eq!(
            FleetStatus::Warn.worst(FleetStatus::Fail),
            FleetStatus::Fail
        );
        assert_eq!(
            FleetStatus::Pass.worst(FleetStatus::Pass),
            FleetStatus::Pass
        );
    }

    #[test]
    fn unknown_propagates_through_worst_of() {
        // Unknown is treated as "worst" so a half-probed row doesn't
        // render Pass while a check is still missing.
        assert_eq!(
            FleetStatus::Pass.worst(FleetStatus::Unknown),
            FleetStatus::Unknown
        );
        assert_eq!(
            FleetStatus::Fail.worst(FleetStatus::Unknown),
            FleetStatus::Unknown
        );
    }

    #[test]
    fn aggregate_healthy_node_is_pass() {
        let row = aggregate(
            "prod".into(),
            "http://prod:1633".into(),
            true,
            12,
            false,
            Some(87),
            Some(86_400 * 30), // 30 days
        );
        assert_eq!(row.status, FleetStatus::Pass);
        assert!(row.why.is_none());
    }

    #[test]
    fn aggregate_zero_peers_is_fail() {
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            false,
            Some(0),
            Some(86_400 * 30),
        );
        assert_eq!(row.status, FleetStatus::Fail);
        assert!(row.why.unwrap().contains("0 peers"));
    }

    #[test]
    fn aggregate_few_peers_is_warn() {
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            false,
            Some(2),
            Some(86_400 * 30),
        );
        assert_eq!(row.status, FleetStatus::Warn);
        assert!(row.why.unwrap().contains("only 2 peers"));
    }

    #[test]
    fn aggregate_warming_up_is_warn() {
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            true,
            Some(87),
            Some(86_400 * 30),
        );
        assert_eq!(row.status, FleetStatus::Warn);
        assert!(row.why.unwrap().contains("warming up"));
    }

    #[test]
    fn aggregate_urgent_stamp_ttl_is_fail() {
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            false,
            Some(87),
            Some(3600), // 1 hour
        );
        assert_eq!(row.status, FleetStatus::Fail);
        assert!(row.why.unwrap().contains("URGENT"));
    }

    #[test]
    fn aggregate_soon_stamp_ttl_is_warn() {
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            false,
            Some(87),
            Some(86_400 * 3), // 3 days
        );
        assert_eq!(row.status, FleetStatus::Warn);
        assert!(row.why.unwrap().contains("plan a topup"));
    }

    #[test]
    fn aggregate_worst_of_multiple_warns() {
        // Warming up AND few peers → still Warn, why pins to the
        // first thing detected (warmup).
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            true,
            Some(2),
            Some(86_400 * 30),
        );
        assert_eq!(row.status, FleetStatus::Warn);
        assert!(row.why.unwrap().contains("warming up"));
    }

    #[test]
    fn aggregate_fail_dominates_warn() {
        // Warming up (Warn) AND 0 peers (Fail) → Fail.
        let row = aggregate(
            "n".into(),
            "u".into(),
            false,
            10,
            true,
            Some(0),
            Some(86_400 * 30),
        );
        assert_eq!(row.status, FleetStatus::Fail);
    }

    #[test]
    fn snapshot_seed_one_row_per_node() {
        let nodes = vec![
            NodeConfig {
                name: "a".into(),
                url: "http://a".into(),
                token: None,
                default: true,
            },
            NodeConfig {
                name: "b".into(),
                url: "http://b".into(),
                token: None,
                default: false,
            },
        ];
        let snap = FleetSnapshot::seed(&nodes);
        assert_eq!(snap.rows.len(), 2);
        assert!(snap.rows.iter().all(|r| r.status == FleetStatus::Unknown));
        assert!(snap.last_update.is_none());
    }

    #[test]
    fn snapshot_counts_partition_correctly() {
        let snap = FleetSnapshot {
            rows: vec![
                FleetRow {
                    status: FleetStatus::Pass,
                    ..unknown_row("a")
                },
                FleetRow {
                    status: FleetStatus::Pass,
                    ..unknown_row("b")
                },
                FleetRow {
                    status: FleetStatus::Warn,
                    ..unknown_row("c")
                },
                FleetRow {
                    status: FleetStatus::Fail,
                    ..unknown_row("d")
                },
                FleetRow {
                    status: FleetStatus::Unknown,
                    ..unknown_row("e")
                },
            ],
            last_update: None,
        };
        assert_eq!(snap.counts(), (2, 1, 1, 1));
    }

    fn unknown_row(name: &str) -> FleetRow {
        FleetRow::unknown(name.into(), "u".into(), false)
    }
}
