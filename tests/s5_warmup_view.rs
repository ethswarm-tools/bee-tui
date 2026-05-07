//! Snapshot tests for S5 Warmup view computation.
//!
//! [`bee_tui::components::warmup::Warmup::view_for`] is the pure
//! function the draw path delegates to. Snapshotting it pins down
//! every step's `StepState` and the per-step detail line across the
//! main warmup phases (cold start → mid-warmup → almost-done →
//! complete). Update via `cargo insta review` after intentional copy
//! edits.

use std::time::{Duration, Instant};

use bee::debug::{Status, Topology};
use bee_tui::components::warmup::Warmup;
use bee_tui::watch::{HealthSnapshot, StampsSnapshot, TopologySnapshot};

fn status_of(
    is_warming_up: bool,
    connected_peers: i64,
    reserve_size_within_radius: i64,
    storage_radius: i64,
) -> Status {
    Status {
        connected_peers,
        reserve_size_within_radius,
        storage_radius,
        committed_depth: storage_radius,
        is_warming_up,
        ..Status::default()
    }
}

fn health_with(status: Status) -> HealthSnapshot {
    HealthSnapshot {
        status: Some(status),
        last_update: Some(Instant::now()),
        ..HealthSnapshot::default()
    }
}

fn topology_with(depth: u8) -> TopologySnapshot {
    TopologySnapshot {
        topology: Some(Topology {
            depth,
            ..Topology::default()
        }),
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

fn stamps_with(count: usize) -> StampsSnapshot {
    use bee::postage::PostageBatch;
    use bee::swarm::BatchId;
    let batches = (0..count)
        .map(|i| PostageBatch {
            batch_id: BatchId::new(&[i as u8; 32]).unwrap(),
            amount: None,
            start: 0,
            owner: String::new(),
            depth: 22,
            bucket_depth: 16,
            immutable: true,
            batch_ttl: 86_400,
            utilization: 0,
            usable: true,
            exists: true,
            label: format!("batch-{i}"),
            block_number: 0,
        })
        .collect();
    StampsSnapshot {
        batches,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_no_snapshots_loaded() {
    // Cold cold start: nothing has polled yet. Every step should be
    // Unknown so the operator sees "we don't know yet" instead of a
    // false "Pending".
    let view = Warmup::view_for(
        &HealthSnapshot::default(),
        &StampsSnapshot::default(),
        &TopologySnapshot::default(),
        None,
        false,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_fresh_warmup_zero_progress() {
    // Status arrived but reports a brand-new node: 0 peers,
    // reserve empty, no batches yet. is_warming_up=true.
    let view = Warmup::view_for(
        &health_with(status_of(true, 0, 0, 0)),
        &stamps_with(0),
        &topology_with(0),
        Some(Duration::from_secs(15)),
        false,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_mid_warmup() {
    // Halfway through bootstrap: 25 peers (out of target 50),
    // 30k of 65k reserve chunks, 487 batches loaded, depth still
    // wobbling.
    let view = Warmup::view_for(
        &health_with(status_of(true, 25, 30_000, 8)),
        &stamps_with(487),
        &topology_with(8),
        Some(Duration::from_secs(12 * 60 + 38)),
        false,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_almost_done_depth_stable() {
    // Bee still says is_warming_up=true but every per-step check is
    // close to / past target. Depth observation window is full and
    // stable.
    let view = Warmup::view_for(
        &health_with(status_of(true, 75, 65_000, 8)),
        &stamps_with(120),
        &topology_with(8),
        Some(Duration::from_secs(28 * 60)),
        true,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_complete() {
    // Bee reports is_warming_up=false; every step latched Done.
    // Elapsed counter frozen at the moment of completion.
    let view = Warmup::view_for(
        &health_with(status_of(false, 87, 65_536, 8)),
        &stamps_with(487),
        &topology_with(8),
        Some(Duration::from_secs(32 * 60 + 12)),
        true,
    );
    insta::assert_debug_snapshot!(view);
}
