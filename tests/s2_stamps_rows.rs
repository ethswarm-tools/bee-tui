//! Snapshot tests for S2 Stamps row computation.
//!
//! [`bee_tui::components::stamps::Stamps::rows_for`] is the pure
//! function the draw path delegates to. Snapshotting it pins down
//! the StampStatus ladder (Pending / Healthy / Skewed / Critical /
//! Expired) plus the truth-telling tooltips operators rely on.
//! Update via `cargo insta review` after intentional copy edits.

use std::time::Instant;

use bee::postage::PostageBatch;
use bee::swarm::BatchId;
use bee_tui::components::stamps::Stamps;
use bee_tui::watch::StampsSnapshot;

#[allow(clippy::too_many_arguments)] // test fixture helper, not API
fn batch(
    id_byte: u8,
    label: &str,
    depth: u8,
    bucket_depth: u8,
    utilization: u32,
    usable: bool,
    immutable: bool,
    batch_ttl: i64,
) -> PostageBatch {
    PostageBatch {
        batch_id: BatchId::new(&[id_byte; 32]).unwrap(),
        amount: None,
        start: 0,
        owner: String::new(),
        depth,
        bucket_depth,
        immutable,
        batch_ttl,
        utilization,
        usable,
        exists: true,
        label: label.into(),
        block_number: 0,
    }
}

fn snapshot_with(batches: Vec<PostageBatch>) -> StampsSnapshot {
    StampsSnapshot {
        batches,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn rows_empty_snapshot() {
    let rows = Stamps::rows_for(&snapshot_with(vec![]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_pending_batch() {
    // Bee has accepted the buy but the chain hasn't confirmed yet.
    // For depth=22, bucketDepth=16: 16 GiB theoretical, 64
    // chunks/bucket cap.
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xaa,
        "fresh-buy",
        22,
        16,
        0,
        false,
        true,
        86_400, // 1d
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_healthy_batch() {
    // 16 GiB batch, low utilization, plenty of TTL, immutable.
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xbb,
        "prod-mainnet",
        22,
        16,
        14, // 14/64 = 22%
        true,
        true,
        47 * 86_400 + 12 * 3_600, // 47d 12h
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_skewed_batch() {
    // Worst bucket at 85% — operator should dilute or stop using.
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xcc,
        "near-full",
        22,
        16,
        55, // 55/64 = 85%
        true,
        true,
        12 * 86_400,
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_critical_immutable_batch() {
    // Worst bucket at ~98%; immutable -> next upload will be REJECTED.
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xdd,
        "about-to-fail",
        22,
        16,
        63, // 63/64 = 98%
        true,
        true,
        5 * 86_400,
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_critical_mutable_batch() {
    // Worst bucket at ~98%; mutable -> silent overwrite of older
    // chunks rather than a hard reject (bee#5334).
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xee,
        "mutable-near-full",
        22,
        16,
        63,
        true,
        false,
        5 * 86_400,
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_expired_batch() {
    // batch_ttl <= 0 — paid balance exhausted.
    let rows = Stamps::rows_for(&snapshot_with(vec![batch(
        0xff, "burned", 22, 16, 30, true, true, 0,
    )]));
    insta::assert_debug_snapshot!(rows);
}

#[test]
fn rows_multi_batch_mixed() {
    // Realistic operator setup: a healthy primary, a skewed
    // secondary, and a still-pending fresh buy.
    let rows = Stamps::rows_for(&snapshot_with(vec![
        batch(0xb1, "prod-mainnet", 22, 16, 14, true, true, 47 * 86_400),
        batch(0xb2, "spillover", 22, 16, 55, true, true, 12 * 86_400),
        batch(0xb3, "fresh-buy", 22, 16, 0, false, true, 86_400),
    ]));
    insta::assert_debug_snapshot!(rows);
}
