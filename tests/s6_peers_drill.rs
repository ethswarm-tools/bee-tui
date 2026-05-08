//! Snapshot tests for the S6 Peers drill pane.
//!
//! [`bee_tui::components::peers::Peers::compute_peer_drill_view`]
//! takes the four-endpoint fan-out result and produces the per-peer
//! summary the drill pane renders. Pinning it via insta locks the
//! per-field formatting + partial-failure handling against future
//! refactors.

use num_bigint::BigInt;

use bee::debug::{Balance, Cheque, PeerCheques, PeerStatus, Settlement, Status};
use bee_tui::components::peers::{PeerDrillFetch, Peers};

fn plur(bzz_hundredths: i64) -> BigInt {
    // BZZ scale is 10^16 PLUR per BZZ. Multiply hundredths by 10^14
    // so the test inputs read like real BZZ amounts.
    BigInt::from(bzz_hundredths) * BigInt::from(10u64).pow(14)
}

#[test]
fn drill_view_realistic_paying_peer() {
    // Operator-relevant shape: a peer we owe (negative balance), a
    // settlement we've received from (cheque inbound), no outbound
    // cheques yet.
    let fetch = PeerDrillFetch {
        balance: Ok(Balance {
            peer: "abc".into(),
            balance: -plur(12), // -0.12 BZZ — we owe them
        }),
        cheques: Ok(PeerCheques {
            peer: "abc".into(),
            last_received: Some(Cheque {
                beneficiary: "0x".into(),
                chequebook: "0x".into(),
                payout: Some(plur(150)), // 1.50 BZZ
            }),
            last_sent: None,
        }),
        settlement: Ok(Settlement {
            peer: "abc".into(),
            received: Some(plur(800)), // 8.00 BZZ
            sent: Some(plur(150)),     // 1.50 BZZ
        }),
        ping: Ok("4.21ms".into()),
        peer_status: Ok(Some(PeerStatus {
            status: Status {
                overlay: "abc".into(),
                storage_radius: 8,
                reserve_size: 420_000,
                pullsync_rate: 12.5,
                batch_commitment: 99_715_645_440,
                ..Status::default()
            },
            request_failed: false,
        })),
        local_status: Ok(Status {
            batch_commitment: 99_715_645_440,
            ..Status::default()
        }),
    };
    let view = Peers::compute_peer_drill_view(
        "abc1234567890abc1234567890abc1234567890abc1234567890abc1234567890",
        Some(7),
        &fetch,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn drill_view_partial_failure() {
    // /chequebook/cheque/{peer} returned 404 (peer never sent us a
    // cheque) — the rest succeeded. The view must still surface
    // balance + settlement + ping, with cheque marked as Err so the
    // operator knows that field tried and failed.
    let fetch = PeerDrillFetch {
        balance: Ok(Balance {
            peer: "x".into(),
            balance: BigInt::from(0),
        }),
        cheques: Err("Response { status: 404, status_text: \"404 Not Found\" }".into()),
        settlement: Ok(Settlement {
            peer: "x".into(),
            received: None,
            sent: None,
        }),
        ping: Ok("12ms".into()),
        peer_status: Ok(None),
        local_status: Err("503".into()),
    };
    let view = Peers::compute_peer_drill_view("xxx", None, &fetch);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn drill_view_all_failed() {
    // Pathological: every endpoint failed (peer just disconnected).
    // View should still render with each field as Err — operator
    // can read the error strings to debug.
    let err = "Response { status: 503, body: \"Node is syncing\" }".to_string();
    let fetch = PeerDrillFetch {
        balance: Err(err.clone()),
        cheques: Err(err.clone()),
        settlement: Err(err.clone()),
        ping: Err(err.clone()),
        peer_status: Err(err.clone()),
        local_status: Err(err),
    };
    let view = Peers::compute_peer_drill_view("zzz", Some(0), &fetch);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn drill_view_outlier_batch_commitment() {
    // Peer reports batch_commitment ~50% off our local view → red,
    // mirroring `bee-scripts/bad-status.sh`.
    let fetch = PeerDrillFetch {
        balance: Ok(Balance {
            peer: "out".into(),
            balance: BigInt::from(0),
        }),
        cheques: Ok(PeerCheques {
            peer: "out".into(),
            last_received: None,
            last_sent: None,
        }),
        settlement: Ok(Settlement {
            peer: "out".into(),
            received: None,
            sent: None,
        }),
        ping: Ok("8ms".into()),
        peer_status: Ok(Some(PeerStatus {
            status: Status {
                overlay: "out".into(),
                storage_radius: 6,
                reserve_size: 80_000,
                pullsync_rate: 1.2,
                batch_commitment: 50_000_000_000,
                ..Status::default()
            },
            request_failed: false,
        })),
        local_status: Ok(Status {
            batch_commitment: 99_715_645_440,
            ..Status::default()
        }),
    };
    let view = Peers::compute_peer_drill_view("out", Some(6), &fetch);
    insta::assert_debug_snapshot!(view);
}
