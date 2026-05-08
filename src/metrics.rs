//! Prometheus exposition-format renderer for the gauges bee-tui
//! already computes. Pure: takes references to the live snapshots
//! and produces the `/metrics` body as a `String`.
//!
//! ## What we emit (and why bother — Bee already has /metrics)
//!
//! Bee's own `/metrics` exposes infrastructure-level counters
//! (peer counts, blocklist size, syncer queue depths). bee-tui's
//! value-adds — the parts no other endpoint exposes — are:
//!
//! - **Per-batch worst-bucket fill** — Bee reports `utilization` but
//!   not the % of bucket capacity, which is the value that actually
//!   predicts upload failure.
//! - **Predictive stamp economics** — depth, capacity bytes, TTL
//!   seconds per batch, rolled up so a Grafana panel can graph
//!   "TTL trajectory across all batches".
//! - **Pending-tx age** — operator-relevant signal that Bee surfaces
//!   only as a creation timestamp (see Tier 1.A).
//! - **Depth-vs-radius gap** — derived from `committed_depth -
//!   storage_radius`; a positive gap means the node is hosting
//!   chunks beyond its storage radius, which is operator-relevant
//!   for chunk-loss risk during shrinkage.
//! - **bee-tui's own request stats** — p50/p99/error-rate over the
//!   client-side log-capture window. Distinguishes "bee is slow"
//!   from "the network between bee-tui and bee is slow".
//!
//! Everything else is included as a courtesy so a Grafana board
//! can aggregate without two scrape sources.
//!
//! ## Format
//!
//! Plain text, one metric per family, `# HELP` and `# TYPE` lines
//! per metric. Label values are escaped per the Prometheus
//! exposition-format spec (`\\`, `\n`, `\"`).
//!
//! ## Why no `prometheus` crate
//!
//! The `prometheus` crate brings ~30 transitive deps and a Registry
//! abstraction we don't need. The format itself is trivial — under
//! 200 lines of glue here. Same "no needless deps" stance as the
//! rest of bee-tui.

use bee::debug::Status;
use bee::postage::PostageBatch;
use std::fmt::Write as _;

use crate::components::api_health::CallStats;
use crate::components::stamps;
use crate::watch::{
    HealthSnapshot, LotterySnapshot, NetworkSnapshot, StampsSnapshot, SwapSnapshot,
    TopologySnapshot, TransactionsSnapshot,
};

/// All snapshot references the renderer needs. Borrowed: the caller
/// owns the originals (they live inside `BeeWatch`'s watch channels)
/// and just hands us a frozen copy via `.borrow().clone()` at scrape
/// time.
#[derive(Clone, Debug)]
pub struct MetricsInputs<'a> {
    pub bee_tui_version: &'a str,
    pub health: &'a HealthSnapshot,
    pub stamps: &'a StampsSnapshot,
    pub swap: &'a SwapSnapshot,
    pub lottery: &'a LotterySnapshot,
    pub topology: &'a TopologySnapshot,
    pub network: &'a NetworkSnapshot,
    pub transactions: &'a TransactionsSnapshot,
    pub call_stats: &'a CallStats,
    /// Wall-clock seconds since the unix epoch, used to compute
    /// pending-tx ages without taking a SystemTime::now() call inside
    /// the renderer (keeps it pure for tests).
    pub now_unix: i64,
}

/// Render the full `/metrics` body. Returns a `String` ready to ship
/// over HTTP with `Content-Type: text/plain; version=0.0.4`.
pub fn render(inputs: &MetricsInputs<'_>) -> String {
    let mut out = String::with_capacity(4096);

    // Liveness + identity.
    emit_help(&mut out, "bee_tui_up", "1 if bee-tui is scraping.");
    emit_type(&mut out, "bee_tui_up", "gauge");
    writeln!(out, "bee_tui_up 1").unwrap();

    let overlay = inputs
        .health
        .status
        .as_ref()
        .map(|s| s.overlay.as_str())
        .unwrap_or("");
    let bee_mode = inputs
        .health
        .status
        .as_ref()
        .map(|s| s.bee_mode.as_str())
        .unwrap_or("");
    emit_help(
        &mut out,
        "bee_tui_info",
        "Build + node identity (always 1).",
    );
    emit_type(&mut out, "bee_tui_info", "gauge");
    writeln!(
        out,
        "bee_tui_info{{version=\"{}\",overlay=\"{}\",bee_mode=\"{}\"}} 1",
        escape_label(inputs.bee_tui_version),
        escape_label(overlay),
        escape_label(bee_mode),
    )
    .unwrap();

    render_status(&mut out, inputs.health.status.as_ref());
    render_chain(&mut out, inputs.health.chain_state.as_ref());
    render_stamps(&mut out, &inputs.stamps.batches);
    render_pending_tx(&mut out, &inputs.transactions.pending, inputs.now_unix);
    render_self_calls(&mut out, inputs.call_stats);
    render_swap(&mut out, inputs.swap);
    render_lottery(&mut out, inputs.lottery);
    render_topology(&mut out, inputs.topology);
    render_network(&mut out, inputs.network);
    render_loaded_flags(&mut out, inputs);

    out
}

fn render_status(out: &mut String, status: Option<&Status>) {
    let Some(s) = status else { return };
    emit_gauge(out, "bee_tui_status_connected_peers", s.connected_peers);
    emit_gauge(out, "bee_tui_status_neighborhood_size", s.neighborhood_size);
    emit_gauge(out, "bee_tui_status_reserve_size_chunks", s.reserve_size);
    emit_gauge(
        out,
        "bee_tui_status_reserve_size_within_radius_chunks",
        s.reserve_size_within_radius,
    );
    emit_gauge(out, "bee_tui_status_storage_radius", s.storage_radius);
    emit_gauge(out, "bee_tui_status_committed_depth", s.committed_depth);
    // Depth-vs-radius gap — a value bee-tui synthesises and Bee's own
    // /metrics never exposes. Positive = node hosting chunks beyond
    // its storage radius (chunk-loss risk if reserve shrinks).
    emit_gauge(
        out,
        "bee_tui_status_depth_radius_gap",
        s.committed_depth - s.storage_radius,
    );
    emit_gauge(
        out,
        "bee_tui_status_is_reachable",
        if s.is_reachable { 1 } else { 0 },
    );
    emit_gauge(
        out,
        "bee_tui_status_is_warming_up",
        if s.is_warming_up { 1 } else { 0 },
    );
    emit_gauge(out, "bee_tui_status_last_synced_block", s.last_synced_block);
    emit_gauge(out, "bee_tui_status_proximity", s.proximity);
    emit_gauge(out, "bee_tui_status_batch_commitment", s.batch_commitment);
    // pullsync_rate is a float — emit verbatim with the `_per_second`
    // suffix to honour the prom convention of unit-suffix-on-name.
    emit_help(
        out,
        "bee_tui_status_pullsync_rate_per_second",
        "Chunks/sec being pull-synced from neighbours.",
    );
    emit_type(out, "bee_tui_status_pullsync_rate_per_second", "gauge");
    writeln!(
        out,
        "bee_tui_status_pullsync_rate_per_second {}",
        s.pullsync_rate
    )
    .unwrap();
}

fn render_chain(out: &mut String, chain: Option<&bee::debug::ChainState>) {
    let Some(c) = chain else { return };
    emit_gauge(out, "bee_tui_chain_block", c.block as i64);
    emit_gauge(out, "bee_tui_chain_tip", c.chain_tip as i64);
    let lag = (c.chain_tip as i64) - (c.block as i64);
    emit_gauge(out, "bee_tui_chain_lag_blocks", lag);
    // current_price is a BigInt; format as decimal string (Prometheus
    // accepts arbitrary numeric tokens).
    emit_help(
        out,
        "bee_tui_chain_current_price_plur",
        "Per-chunk PLUR/block price from /chain-state.",
    );
    emit_type(out, "bee_tui_chain_current_price_plur", "gauge");
    writeln!(out, "bee_tui_chain_current_price_plur {}", c.current_price).unwrap();
}

fn render_stamps(out: &mut String, batches: &[PostageBatch]) {
    emit_gauge(out, "bee_tui_stamps_count", batches.len() as i64);

    if batches.is_empty() {
        return;
    }

    // Per-batch gauges. Use the same row computation S2 uses so the
    // worst_bucket_pct here matches what operators see in the table.
    emit_help(
        out,
        "bee_tui_stamp_worst_bucket_ratio",
        "Worst-bucket fill as a 0..1 ratio (S2's worst-bucket %).",
    );
    emit_type(out, "bee_tui_stamp_worst_bucket_ratio", "gauge");
    emit_help(
        out,
        "bee_tui_stamp_ttl_seconds",
        "Predicted TTL for the batch in seconds.",
    );
    emit_type(out, "bee_tui_stamp_ttl_seconds", "gauge");
    emit_help(out, "bee_tui_stamp_depth", "Batch depth.");
    emit_type(out, "bee_tui_stamp_depth", "gauge");
    emit_help(
        out,
        "bee_tui_stamp_capacity_bytes",
        "Theoretical capacity = 2^depth * 4096.",
    );
    emit_type(out, "bee_tui_stamp_capacity_bytes", "gauge");
    emit_help(
        out,
        "bee_tui_stamp_immutable",
        "1 if the batch is immutable, 0 if mutable.",
    );
    emit_type(out, "bee_tui_stamp_immutable", "gauge");
    emit_help(
        out,
        "bee_tui_stamp_usable",
        "1 if Bee considers the batch usable (chain-confirmed).",
    );
    emit_type(out, "bee_tui_stamp_usable", "gauge");

    let rows = stamps::Stamps::rows_for(&StampsSnapshot {
        batches: batches.to_vec(),
        ..Default::default()
    });
    for (b, row) in batches.iter().zip(rows.iter()) {
        let id = b.batch_id.to_hex();
        let labels = format!(
            "{{batch_id=\"{}\",label=\"{}\"}}",
            escape_label(&id),
            escape_label(&row.label),
        );
        let ratio = (row.worst_bucket_pct as f64) / 100.0;
        writeln!(out, "bee_tui_stamp_worst_bucket_ratio{labels} {ratio}").unwrap();
        writeln!(out, "bee_tui_stamp_ttl_seconds{labels} {}", b.batch_ttl).unwrap();
        writeln!(out, "bee_tui_stamp_depth{labels} {}", b.depth).unwrap();
        let capacity_bytes: u128 = (1u128 << b.depth) * 4096;
        writeln!(out, "bee_tui_stamp_capacity_bytes{labels} {capacity_bytes}").unwrap();
        writeln!(
            out,
            "bee_tui_stamp_immutable{labels} {}",
            if b.immutable { 1 } else { 0 }
        )
        .unwrap();
        writeln!(
            out,
            "bee_tui_stamp_usable{labels} {}",
            if b.usable { 1 } else { 0 }
        )
        .unwrap();
    }
}

fn render_pending_tx(out: &mut String, pending: &[bee::debug::TransactionInfo], now_unix: i64) {
    emit_gauge(out, "bee_tui_pending_tx_count", pending.len() as i64);
    if pending.is_empty() {
        return;
    }
    let oldest = pending
        .iter()
        .filter_map(|t| {
            crate::components::api_health::parse_rfc3339_to_unix(&t.created).map(|ts| now_unix - ts)
        })
        .max()
        .unwrap_or(0);
    emit_gauge(out, "bee_tui_pending_tx_oldest_age_seconds", oldest);
}

fn render_self_calls(out: &mut String, stats: &CallStats) {
    emit_gauge(
        out,
        "bee_tui_self_request_sample_size",
        stats.sample_size as i64,
    );
    if let Some(p50) = stats.p50_ms {
        emit_help(
            out,
            "bee_tui_self_request_latency_p50_seconds",
            "Median request latency over the recent window.",
        );
        emit_type(out, "bee_tui_self_request_latency_p50_seconds", "gauge");
        writeln!(
            out,
            "bee_tui_self_request_latency_p50_seconds {}",
            (p50 as f64) / 1000.0
        )
        .unwrap();
    }
    if let Some(p99) = stats.p99_ms {
        emit_help(
            out,
            "bee_tui_self_request_latency_p99_seconds",
            "99th-percentile request latency.",
        );
        emit_type(out, "bee_tui_self_request_latency_p99_seconds", "gauge");
        writeln!(
            out,
            "bee_tui_self_request_latency_p99_seconds {}",
            (p99 as f64) / 1000.0
        )
        .unwrap();
    }
    emit_help(
        out,
        "bee_tui_self_request_error_ratio",
        "Fraction of recent requests with status >= 400 (0..1).",
    );
    emit_type(out, "bee_tui_self_request_error_ratio", "gauge");
    writeln!(
        out,
        "bee_tui_self_request_error_ratio {}",
        stats.error_rate_pct / 100.0
    )
    .unwrap();
}

fn render_swap(out: &mut String, swap: &SwapSnapshot) {
    if let Some(c) = &swap.chequebook {
        emit_help(
            out,
            "bee_tui_swap_chequebook_total_plur",
            "Total chequebook balance in PLUR.",
        );
        emit_type(out, "bee_tui_swap_chequebook_total_plur", "gauge");
        writeln!(
            out,
            "bee_tui_swap_chequebook_total_plur {}",
            c.total_balance
        )
        .unwrap();
        emit_help(
            out,
            "bee_tui_swap_chequebook_available_plur",
            "Available (uncashed) chequebook balance in PLUR.",
        );
        emit_type(out, "bee_tui_swap_chequebook_available_plur", "gauge");
        writeln!(
            out,
            "bee_tui_swap_chequebook_available_plur {}",
            c.available_balance
        )
        .unwrap();
    }
}

fn render_lottery(out: &mut String, lottery: &LotterySnapshot) {
    if let Some(staked) = &lottery.staked {
        emit_help(
            out,
            "bee_tui_lottery_staked_plur",
            "Currently staked BZZ in PLUR.",
        );
        emit_type(out, "bee_tui_lottery_staked_plur", "gauge");
        writeln!(out, "bee_tui_lottery_staked_plur {staked}").unwrap();
    }
}

fn render_topology(out: &mut String, topology: &TopologySnapshot) {
    if let Some(t) = &topology.topology {
        emit_gauge(out, "bee_tui_topology_population", t.population);
        emit_gauge(out, "bee_tui_topology_connected", t.connected);
        emit_gauge(out, "bee_tui_topology_depth", i64::from(t.depth));
        emit_gauge(out, "bee_tui_topology_radius", t.nn_low_watermark);
    }
}

fn render_network(out: &mut String, network: &NetworkSnapshot) {
    if let Some(a) = &network.addresses {
        // Underlay-multiaddr count is a useful health proxy: a node
        // with 0 underlay addresses won't be reachable for incoming
        // connections regardless of what /readiness says.
        emit_gauge(
            out,
            "bee_tui_network_underlay_count",
            a.underlay.len() as i64,
        );
    }
}

fn render_loaded_flags(out: &mut String, inputs: &MetricsInputs<'_>) {
    // Per-resource freshness — 1 = last poll succeeded, 0 = error or
    // not yet loaded. Lets a Grafana alert distinguish "bee-tui can't
    // reach Bee" from "stamps screen specifically failed".
    emit_help(
        out,
        "bee_tui_resource_loaded",
        "1 if the resource's most recent poll succeeded.",
    );
    emit_type(out, "bee_tui_resource_loaded", "gauge");
    let pairs: [(&str, bool); 7] = [
        ("health", inputs.health.is_fully_loaded()),
        ("stamps", inputs.stamps.is_loaded()),
        ("swap", inputs.swap.is_loaded()),
        ("lottery", inputs.lottery.is_loaded()),
        ("topology", inputs.topology.is_loaded()),
        ("network", inputs.network.is_loaded()),
        ("transactions", inputs.transactions.is_loaded()),
    ];
    for (name, loaded) in pairs {
        writeln!(
            out,
            "bee_tui_resource_loaded{{resource=\"{name}\"}} {}",
            if loaded { 1 } else { 0 }
        )
        .unwrap();
    }
}

fn emit_help(out: &mut String, name: &str, help: &str) {
    writeln!(out, "# HELP {name} {help}").unwrap();
}

fn emit_type(out: &mut String, name: &str, ty: &str) {
    writeln!(out, "# TYPE {name} {ty}").unwrap();
}

fn emit_gauge(out: &mut String, name: &str, value: i64) {
    emit_type(out, name, "gauge");
    writeln!(out, "{name} {value}").unwrap();
}

/// Escape a label value per the Prometheus exposition format
/// (https://prometheus.io/docs/instrumenting/exposition_formats/):
/// `\\`, `\n`, `\"`.
pub fn escape_label(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            other => out.push(other),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use bee::debug::{ChainState, Status};
    use num_bigint::BigInt;

    fn empty_inputs() -> (
        HealthSnapshot,
        StampsSnapshot,
        SwapSnapshot,
        LotterySnapshot,
        TopologySnapshot,
        NetworkSnapshot,
        TransactionsSnapshot,
        CallStats,
    ) {
        (
            HealthSnapshot::default(),
            StampsSnapshot::default(),
            SwapSnapshot::default(),
            LotterySnapshot::default(),
            TopologySnapshot::default(),
            NetworkSnapshot::default(),
            TransactionsSnapshot::default(),
            CallStats {
                sample_size: 0,
                p50_ms: None,
                p99_ms: None,
                error_rate_pct: 0.0,
            },
        )
    }

    #[test]
    fn empty_render_includes_only_baseline_metrics() {
        let (h, s, sw, l, t, n, tx, cs) = empty_inputs();
        let inputs = MetricsInputs {
            bee_tui_version: "1.0.0",
            health: &h,
            stamps: &s,
            swap: &sw,
            lottery: &l,
            topology: &t,
            network: &n,
            transactions: &tx,
            call_stats: &cs,
            now_unix: 1_700_000_000,
        };
        let body = render(&inputs);
        assert!(body.contains("bee_tui_up 1"));
        assert!(body.contains("bee_tui_info{"));
        assert!(body.contains("bee_tui_stamps_count 0"));
        assert!(body.contains("bee_tui_pending_tx_count 0"));
        // No status info → no per-status gauges.
        assert!(!body.contains("bee_tui_status_connected_peers"));
        // No chain state → no chain gauges.
        assert!(!body.contains("bee_tui_chain_block"));
    }

    #[test]
    fn full_render_includes_status_chain_and_loaded_flags() {
        let h = HealthSnapshot {
            status: Some(Status {
                overlay: "abc".into(),
                proximity: 8,
                bee_mode: "full".into(),
                reserve_size: 1234,
                reserve_size_within_radius: 1000,
                pullsync_rate: 12.5,
                storage_radius: 4,
                connected_peers: 42,
                neighborhood_size: 3,
                batch_commitment: 7,
                is_reachable: true,
                last_synced_block: 100,
                committed_depth: 6,
                is_warming_up: false,
            }),
            chain_state: Some(ChainState {
                block: 95,
                chain_tip: 100,
                current_price: BigInt::from(42_u32),
                total_amount: BigInt::from(1000_u32),
            }),
            ..HealthSnapshot::default()
        };

        let (_, s, sw, l, t, n, tx, cs) = empty_inputs();
        let inputs = MetricsInputs {
            bee_tui_version: "test",
            health: &h,
            stamps: &s,
            swap: &sw,
            lottery: &l,
            topology: &t,
            network: &n,
            transactions: &tx,
            call_stats: &cs,
            now_unix: 1_700_000_000,
        };
        let body = render(&inputs);
        // Status fields rendered.
        assert!(body.contains("bee_tui_status_connected_peers 42"));
        assert!(body.contains("bee_tui_status_storage_radius 4"));
        assert!(body.contains("bee_tui_status_committed_depth 6"));
        assert!(body.contains("bee_tui_status_depth_radius_gap 2")); // 6 - 4
        assert!(body.contains("bee_tui_status_is_reachable 1"));
        assert!(body.contains("bee_tui_status_pullsync_rate_per_second 12.5"));
        // Chain fields rendered.
        assert!(body.contains("bee_tui_chain_block 95"));
        assert!(body.contains("bee_tui_chain_tip 100"));
        assert!(body.contains("bee_tui_chain_lag_blocks 5"));
        assert!(body.contains("bee_tui_chain_current_price_plur 42"));
        // Loaded flags emitted.
        assert!(body.contains("bee_tui_resource_loaded{resource=\"health\"}"));
        assert!(body.contains("bee_tui_resource_loaded{resource=\"stamps\"} 0"));
    }

    #[test]
    fn label_escaping_handles_quotes_backslash_newlines() {
        assert_eq!(escape_label(r#"a"b\c"#), r#"a\"b\\c"#);
        assert_eq!(escape_label("a\nb"), r"a\nb");
        assert_eq!(escape_label("plain"), "plain");
    }

    fn make_tx(created: &str) -> bee::debug::TransactionInfo {
        bee::debug::TransactionInfo {
            transaction_hash: String::new(),
            to: String::new(),
            nonce: 0,
            gas_price: None,
            gas_limit: 0,
            gas_tip_boost: 0,
            gas_tip_cap: None,
            gas_fee_cap: None,
            data: String::new(),
            created: created.into(),
            description: String::new(),
            value: None,
        }
    }

    #[test]
    fn pending_tx_oldest_age_uses_max_not_first() {
        // Two pendings: 60s and 600s old. We should report 600.
        let txs = vec![
            make_tx("2026-05-08T11:59:00Z"),
            make_tx("2026-05-08T11:50:00Z"),
        ];
        let now =
            crate::components::api_health::parse_rfc3339_to_unix("2026-05-08T12:00:00Z").unwrap();
        let tx_snap = TransactionsSnapshot {
            pending: txs,
            ..TransactionsSnapshot::default()
        };

        let (h, s, sw, l, t, n, _, cs) = empty_inputs();
        let inputs = MetricsInputs {
            bee_tui_version: "test",
            health: &h,
            stamps: &s,
            swap: &sw,
            lottery: &l,
            topology: &t,
            network: &n,
            transactions: &tx_snap,
            call_stats: &cs,
            now_unix: now,
        };
        let body = render(&inputs);
        assert!(body.contains("bee_tui_pending_tx_count 2"));
        assert!(body.contains("bee_tui_pending_tx_oldest_age_seconds 600"));
    }

    #[test]
    fn stamp_metrics_have_per_batch_labels() {
        use bee::postage::PostageBatch;
        use bee::swarm::BatchId;

        let batches = StampsSnapshot {
            batches: vec![PostageBatch {
                batch_id: BatchId::new(&[0xab; 32]).unwrap(),
                amount: None,
                start: 0,
                owner: "".into(),
                depth: 22,
                bucket_depth: 16,
                immutable: true,
                batch_ttl: 86_400,
                utilization: 32, // 50% of 64 → ratio 0.5
                usable: true,
                exists: true,
                label: "prod".into(),
                block_number: 0,
            }],
            ..StampsSnapshot::default()
        };

        let (h, _, sw, l, t, n, tx, cs) = empty_inputs();
        let inputs = MetricsInputs {
            bee_tui_version: "test",
            health: &h,
            stamps: &batches,
            swap: &sw,
            lottery: &l,
            topology: &t,
            network: &n,
            transactions: &tx,
            call_stats: &cs,
            now_unix: 0,
        };
        let body = render(&inputs);
        assert!(body.contains("bee_tui_stamps_count 1"));
        assert!(body.contains("batch_id=\"abababab"));
        assert!(body.contains("label=\"prod\""));
        assert!(body.contains("bee_tui_stamp_worst_bucket_ratio"));
        assert!(body.contains("bee_tui_stamp_depth"));
        // 16 GiB at depth 22.
        assert!(body.contains("bee_tui_stamp_capacity_bytes"));
        assert!(body.contains("17179869184"));
    }
}
