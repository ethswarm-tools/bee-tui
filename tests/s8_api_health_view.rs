//! Snapshot tests for S8 RPC / API health view computation.
//!
//! [`bee_tui::components::api_health::ApiHealth::view_for`] is the
//! pure function the draw path delegates to. Snapshotting it pins
//! down the call-stat percentile math (empty / all-success / mixed /
//! windowed), the chain-state delta extraction, and the pending-tx
//! row formatting.

use std::time::Instant;

use bee::debug::{ChainState, TransactionInfo};
use bee_tui::components::api_health::ApiHealth;
use bee_tui::log_capture::LogEntry;
use bee_tui::watch::{HealthSnapshot, TransactionsSnapshot};

fn entry(method: &str, status: Option<u16>, elapsed_ms: Option<u64>) -> LogEntry {
    LogEntry {
        ts: String::new(),
        method: method.into(),
        url: "http://localhost:1633/x".into(),
        status,
        elapsed_ms,
        message: String::new(),
    }
}

fn parse_chain_state(json: &str) -> ChainState {
    serde_json::from_str(json).expect("valid ChainState JSON")
}

fn health_with_chain(json: &str) -> HealthSnapshot {
    HealthSnapshot {
        chain_state: Some(parse_chain_state(json)),
        last_update: Some(Instant::now()),
        ..HealthSnapshot::default()
    }
}

fn pending_tx(nonce: u64, hash_byte: u8, to_byte: u8, description: &str) -> TransactionInfo {
    TransactionInfo {
        transaction_hash: format!("0x{}", format!("{:02x}", hash_byte).repeat(32)),
        to: format!("0x{}", format!("{:02x}", to_byte).repeat(20)),
        nonce,
        gas_price: None,
        gas_limit: 0,
        gas_tip_boost: 0,
        gas_tip_cap: None,
        gas_fee_cap: None,
        data: String::new(),
        created: "2024-05-07T12:34:56Z".into(),
        description: description.into(),
        value: None,
    }
}

fn transactions_with(pending: Vec<TransactionInfo>) -> TransactionsSnapshot {
    TransactionsSnapshot {
        pending,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_no_data() {
    let view = ApiHealth::view_for(
        "http://localhost:1633",
        &[],
        &HealthSnapshot::default(),
        &TransactionsSnapshot::default(),
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_all_successful_calls() {
    // 100 successful calls with deterministic latencies 1..=100 ms.
    // p50 → 50, p99 → 99, error rate 0.
    let entries: Vec<LogEntry> = (1..=100)
        .map(|i| entry("GET", Some(200), Some(i)))
        .collect();
    let view = ApiHealth::view_for(
        "http://localhost:1633",
        &entries,
        &health_with_chain(r#"{"block":12345,"chainTip":12347,"currentPrice":"42","totalAmount":"1000"}"#),
        &TransactionsSnapshot::default(),
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_mixed_errors() {
    let mut entries: Vec<LogEntry> = (1..=10)
        .map(|i| entry("GET", Some(200), Some(i * 10)))
        .collect();
    // 2 of 12 entries are errors → ~16.67% error rate.
    entries.push(entry("POST", Some(500), Some(50)));
    entries.push(entry("POST", Some(404), Some(15)));
    let view = ApiHealth::view_for(
        "http://localhost:1633",
        &entries,
        &health_with_chain(r#"{"block":100,"chainTip":100,"currentPrice":"0","totalAmount":"0"}"#),
        &TransactionsSnapshot::default(),
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_pending_transactions() {
    let txs = vec![
        pending_tx(142, 0xab, 0xcd, "stake deposit"),
        pending_tx(143, 0xef, 0x12, "postage topup"),
        pending_tx(144, 0x34, 0x56, ""),
    ];
    let view = ApiHealth::view_for(
        "http://10.0.1.5:1633",
        &[],
        &health_with_chain(r#"{"block":777,"chainTip":777,"currentPrice":"100","totalAmount":"5000"}"#),
        &transactions_with(txs),
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_chain_lagging() {
    // Bee's local block is 50 behind the chain tip — Δ = +50.
    let view = ApiHealth::view_for(
        "http://localhost:1633",
        &[],
        &health_with_chain(r#"{"block":1000,"chainTip":1050,"currentPrice":"42","totalAmount":"1000"}"#),
        &TransactionsSnapshot::default(),
    );
    insta::assert_debug_snapshot!(view);
}
