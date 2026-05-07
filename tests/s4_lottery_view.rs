//! Snapshot tests for S4 Lottery view computation.
//!
//! [`bee_tui::components::lottery::Lottery::view_for`] is the pure
//! function the draw path delegates to. Snapshotting it pins down the
//! phase-segment ladder, anchor `Δ` math, the StakeStatus reasoning
//! tree (which boolean combination produces which tooltip), and the
//! signed reward formatting. Update via `cargo insta review` after
//! intentional copy edits.

use std::time::{Duration, Instant};

use bee::debug::RedistributionState;
use bee_tui::components::lottery::Lottery;
use bee_tui::watch::{HealthSnapshot, LotterySnapshot};
use num_bigint::BigInt;

/// 1 BZZ = 10^16 PLUR.
fn bzz(n: u64) -> BigInt {
    BigInt::from(n) * BigInt::from(10u64).pow(16)
}

fn rs(
    round: u64,
    block: u64,
    phase: &str,
    last_won: u64,
    last_played: u64,
    last_selected: u64,
    last_frozen: u64,
    is_frozen: bool,
    is_healthy: bool,
    has_funds: bool,
    is_synced: bool,
) -> RedistributionState {
    RedistributionState {
        minimum_gas_funds: Some(bzz(2)),
        has_sufficient_funds: has_funds,
        is_frozen,
        is_fully_synced: is_synced,
        phase: phase.into(),
        round,
        last_won_round: last_won,
        last_played_round: last_played,
        last_frozen_round: last_frozen,
        last_selected_round: last_selected,
        last_sample_duration_seconds: 18.4,
        block,
        reward: Some(bzz(142)),
        fees: Some(bzz(3)),
        is_healthy,
    }
}

fn health_with(state: RedistributionState) -> HealthSnapshot {
    HealthSnapshot {
        redistribution: Some(state),
        last_ping: Some(Duration::from_millis(2)),
        last_update: Some(Instant::now()),
        ..HealthSnapshot::default()
    }
}

fn lot_with(staked: Option<BigInt>) -> LotterySnapshot {
    LotterySnapshot {
        staked,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_empty_snapshot() {
    let view = Lottery::view_for(&HealthSnapshot::default(), &LotterySnapshot::default());
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_commit_phase_healthy() {
    // Round 14_528, block 14_528*152 + 10 → block-of-round 10 (commit).
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 14_524, 14_525, 14_527, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_reveal_phase_healthy() {
    let block = 14_528 * 152 + 50;
    let r = rs(
        14_528, block, "reveal", 14_524, 14_527, 14_527, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_claim_phase_healthy() {
    let block = 14_528 * 152 + 100;
    let r = rs(
        14_528, block, "claim", 14_524, 14_528, 14_528, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_sample_between_rounds() {
    // phase=sample → all three on-chain phases read as Done.
    let block = 14_528 * 152 + 151;
    let r = rs(
        14_528, block, "sample", 14_524, 14_528, 14_528, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_unstaked() {
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 0, 0, 0, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(BigInt::from(0))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_frozen() {
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 14_510, 14_520, 14_524, 14_523, true, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_insufficient_gas() {
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 14_510, 14_520, 14_525, 0, false, true, false, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_unhealthy_not_synced() {
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 14_510, 14_520, 14_525, 0, false, true, true, false,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_anchors_never_and_recent() {
    // Mix: "last won" never reached (0); others recent.
    let block = 14_528 * 152 + 10;
    let r = rs(
        14_528, block, "commit", 0, 14_527, 14_528, 0, false, true, true, true,
    );
    let view = Lottery::view_for(&health_with(r), &lot_with(Some(bzz(10))));
    insta::assert_debug_snapshot!(view);
}
