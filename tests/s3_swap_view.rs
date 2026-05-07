//! Snapshot tests for S3 SWAP view computation.
//!
//! [`bee_tui::components::swap::Swap::view_for`] is the pure function
//! the draw path delegates to. Snapshotting it pins down the
//! chequebook card status (Empty / Healthy / Tight / Unknown), the
//! PLUR → BZZ formatting (4 decimal places, signed for net), and the
//! sort order — peers with cheques before peers without, biggest net
//! out-of-balance settlement floats to the top.
//! Update via `cargo insta review` after intentional copy edits.

use std::time::Instant;

use bee::debug::{Cheque, ChequebookBalance, LastCheque, Settlement, Settlements};
use bee_tui::components::swap::Swap;
use bee_tui::watch::SwapSnapshot;
use num_bigint::BigInt;

/// 1 BZZ = 10^16 PLUR.
fn bzz(n: u64) -> BigInt {
    BigInt::from(n) * BigInt::from(10u64).pow(16)
}

/// Build a fractional BZZ amount as PLUR. `tenths` is in 1/10 BZZ units.
fn bzz_tenths(tenths: u64) -> BigInt {
    BigInt::from(tenths) * BigInt::from(10u64).pow(15)
}

fn cheque(beneficiary: &str, payout: BigInt) -> Cheque {
    Cheque {
        beneficiary: beneficiary.into(),
        chequebook: "0x0000000000000000000000000000000000000000".into(),
        payout: Some(payout),
    }
}

fn last_cheque(peer: &str, payout: Option<BigInt>) -> LastCheque {
    LastCheque {
        peer: peer.into(),
        last_received: payout.map(|p| cheque(peer, p)),
    }
}

fn settlement(peer: &str, received: BigInt, sent: BigInt) -> Settlement {
    Settlement {
        peer: peer.into(),
        received: Some(received),
        sent: Some(sent),
    }
}

fn snapshot_with(
    chequebook: Option<ChequebookBalance>,
    last_received: Vec<LastCheque>,
    settlements: Option<Settlements>,
    time_settlements: Option<Settlements>,
) -> SwapSnapshot {
    SwapSnapshot {
        chequebook,
        chequebook_address: None,
        settlements,
        time_settlements,
        last_received,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_empty_snapshot() {
    let view = Swap::view_for(&snapshot_with(None, vec![], None, None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_propagates_chequebook_address() {
    // The address field flows through the view straight from the
    // snapshot — guards against accidentally dropping it during
    // future view_for refactors.
    let mut snap = snapshot_with(None, vec![], None, None);
    snap.chequebook_address = Some("0xCE3EE0201A1A8296E8bC2BE9f912eC21708fd615".into());
    let view = Swap::view_for(&snap);
    assert_eq!(
        view.chequebook_address.as_deref(),
        Some("0xCE3EE0201A1A8296E8bC2BE9f912eC21708fd615"),
    );
}

#[test]
fn view_unfunded_chequebook() {
    let cb = ChequebookBalance {
        total_balance: BigInt::from(0),
        available_balance: BigInt::from(0),
    };
    let view = Swap::view_for(&snapshot_with(Some(cb), vec![], None, None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_healthy_chequebook() {
    let cb = ChequebookBalance {
        total_balance: bzz(10),
        available_balance: bzz(8),
    };
    let view = Swap::view_for(&snapshot_with(Some(cb), vec![], None, None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_tight_chequebook() {
    // Only 10% available — most BZZ is tied up in unsettled debt.
    let cb = ChequebookBalance {
        total_balance: bzz(10),
        available_balance: bzz_tenths(10), // 1.0 BZZ
    };
    let view = Swap::view_for(&snapshot_with(Some(cb), vec![], None, None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_cheques_sort_descending_with_never_at_bottom() {
    let cb = ChequebookBalance {
        total_balance: bzz(10),
        available_balance: bzz(8),
    };
    let cheques = vec![
        last_cheque("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", Some(bzz_tenths(3))),
        last_cheque("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", None),
        last_cheque("0xcccccccccccccccccccccccccccccccc", Some(bzz_tenths(15))),
        last_cheque("0xdddddddddddddddddddddddddddddddd", Some(bzz_tenths(7))),
    ];
    let view = Swap::view_for(&snapshot_with(Some(cb), cheques, None, None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_settlements_with_flagged_peer() {
    let cb = ChequebookBalance {
        total_balance: bzz(10),
        available_balance: bzz(8),
    };
    let s = Settlements {
        total_received: Some(bzz_tenths(25)), // 2.5 BZZ total received
        total_sent: Some(bzz_tenths(5)),      // 0.5 BZZ total sent
        settlements: vec![
            // Steady-state peer: tiny net.
            settlement(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                bzz_tenths(3),
                bzz_tenths(2),
            ),
            // Out-of-balance peer: +1.5 BZZ — flagged.
            settlement(
                "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                bzz_tenths(20),
                bzz_tenths(5),
            ),
            // We owe this one: -0.8 BZZ — flagged on the other side.
            settlement(
                "0xcccccccccccccccccccccccccccccccc",
                bzz_tenths(2),
                bzz_tenths(10),
            ),
        ],
    };
    let view = Swap::view_for(&snapshot_with(Some(cb), vec![], Some(s), None));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_full_realistic() {
    // Complete operator setup: funded chequebook, mixed cheques, mixed
    // settlements, and time-based totals from `/timesettlements`.
    let cb = ChequebookBalance {
        total_balance: bzz(50),
        available_balance: bzz(35),
    };
    let cheques = vec![
        last_cheque("0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa", Some(bzz_tenths(12))),
        last_cheque("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb", Some(bzz_tenths(4))),
        last_cheque("0xcccccccccccccccccccccccccccccccc", None),
    ];
    let settlements = Settlements {
        total_received: Some(bzz_tenths(40)),
        total_sent: Some(bzz_tenths(8)),
        settlements: vec![
            settlement(
                "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                bzz_tenths(25),
                bzz_tenths(3),
            ),
            settlement(
                "0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
                bzz_tenths(15),
                bzz_tenths(5),
            ),
        ],
    };
    let time_settlements = Settlements {
        total_received: Some(bzz_tenths(7)),
        total_sent: Some(bzz_tenths(2)),
        settlements: vec![],
    };
    let view = Swap::view_for(&snapshot_with(
        Some(cb),
        cheques,
        Some(settlements),
        Some(time_settlements),
    ));
    insta::assert_debug_snapshot!(view);
}
