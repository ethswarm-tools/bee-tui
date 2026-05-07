//! Snapshot tests for S1 Health gate computation.
//!
//! [`bee_tui::components::health::Health::gates_for`] is the pure,
//! snapshot-driven function the draw path delegates to; capturing
//! its output guards every gate's status / value / why string against
//! unintended regressions. Update via `cargo insta review` after
//! intentional changes.

use std::time::{Duration, Instant};

use bee::debug::{ChainState, RedistributionState, Status, Wallet};
use bee_tui::components::health::Health;
use bee_tui::watch::HealthSnapshot;

fn parse_chain_state(json: &str) -> ChainState {
    serde_json::from_str(json).expect("valid ChainState JSON")
}

fn parse_wallet(json: &str) -> Wallet {
    serde_json::from_str(json).expect("valid Wallet JSON")
}

fn empty_snapshot() -> HealthSnapshot {
    HealthSnapshot::default()
}

fn fully_loaded_passing_snapshot() -> HealthSnapshot {
    let status = Status {
        connected_peers: 87,
        reserve_size: 145_231,
        reserve_size_within_radius: 65_536,
        storage_radius: 8,
        committed_depth: 8,
        is_warming_up: false,
        ..Status::default()
    };

    let redist = RedistributionState {
        is_healthy: true,
        is_frozen: false,
        has_sufficient_funds: true,
        ..RedistributionState::default()
    };

    HealthSnapshot {
        status: Some(status),
        chain_state: Some(parse_chain_state(r#"{"block":100,"chainTip":100}"#)),
        wallet: Some(parse_wallet(
            r#"{
                "bzzBalance": "1000000000000000000",
                "nativeTokenBalance": "5000000000000000000",
                "chainID": 11155111,
                "walletAddress": "0x56250aef268fded8c33ca70eca851fba9fb94c65"
            }"#,
        )),
        redistribution: Some(redist),
        last_ping: Some(Duration::from_millis(3)),
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

fn unhealthy_snapshot() -> HealthSnapshot {
    let status = Status {
        connected_peers: 3,
        reserve_size: 0,
        reserve_size_within_radius: 0,
        storage_radius: 7,
        committed_depth: 9,
        is_warming_up: false,
        ..Status::default()
    };

    let redist = RedistributionState {
        is_healthy: false,
        is_frozen: true,
        last_frozen_round: 14_523,
        has_sufficient_funds: false,
        ..RedistributionState::default()
    };

    HealthSnapshot {
        status: Some(status),
        chain_state: Some(parse_chain_state(r#"{"block":100,"chainTip":150}"#)),
        wallet: Some(parse_wallet(
            r#"{
                "bzzBalance": "0",
                "nativeTokenBalance": "0",
                "chainID": 11155111,
                "walletAddress": "0x56250aef268fded8c33ca70eca851fba9fb94c65"
            }"#,
        )),
        redistribution: Some(redist),
        last_ping: Some(Duration::from_millis(380)),
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn gates_empty_snapshot() {
    let gates = Health::gates_for(&empty_snapshot());
    insta::assert_debug_snapshot!(gates);
}

#[test]
fn gates_fully_loaded_passing_snapshot() {
    let gates = Health::gates_for(&fully_loaded_passing_snapshot());
    insta::assert_debug_snapshot!(gates);
}

#[test]
fn gates_unhealthy_snapshot() {
    let gates = Health::gates_for(&unhealthy_snapshot());
    insta::assert_debug_snapshot!(gates);
}
