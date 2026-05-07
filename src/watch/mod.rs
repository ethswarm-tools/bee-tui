#![allow(dead_code)] // wired into App + Health screen in the next commits.

//! k9s-style watch / informer layer.
//!
//! One [`BeeWatch`] hub spawns a polling task per resource group;
//! each task pushes fresh snapshots into a [`tokio::sync::watch`]
//! channel. Screens subscribe via [`watch::Receiver`] handles and
//! render the latest snapshot — they never poll directly.
//!
//! The cancellation tree mirrors `docs/PLAN.md` § 6: every poller's
//! token is a child of the hub's, which is a child of the App's
//! root. Quitting cancels the root and unwinds everything; switching
//! profile (v0.4) drops one hub and starts another.
//!
//! Refresh policy is per resource group, not global — `tig`-style
//! (`docs/PLAN.md` § 3 principle 7).

use std::sync::Arc;
use std::time::{Duration, Instant};

use bee::debug::{
    ChainState, ChequebookBalance, LastCheque, RedistributionState, Settlements, Status, Wallet,
};
use bee::postage::PostageBatch;
use tokio::sync::watch;
use tokio_util::sync::CancellationToken;

use crate::api::ApiClient;

/// Snapshot fed to the Health screen and the connection-status bar.
/// Updated together because the gates need a coherent view across
/// `/status`, `/chainstate`, `/wallet`, and `/redistributionstate`.
#[derive(Clone, Debug, Default)]
pub struct HealthSnapshot {
    pub status: Option<Status>,
    pub chain_state: Option<ChainState>,
    pub wallet: Option<Wallet>,
    pub redistribution: Option<RedistributionState>,
    /// Round-trip time of the last `/health` ping; `None` until the
    /// first poll completes or after a transport failure.
    pub last_ping: Option<Duration>,
    /// One-line description of the most recent fetch error, if any.
    /// Cleared on every successful refresh.
    pub last_error: Option<String>,
    /// Wall-clock instant of the last successful poll. Used to grey
    /// out stale data when the link drops.
    pub last_update: Option<Instant>,
}

impl HealthSnapshot {
    /// True iff every required field is populated and there is no
    /// recorded error. Used by the connection-status indicator.
    pub fn is_fully_loaded(&self) -> bool {
        self.last_error.is_none()
            && self.status.is_some()
            && self.chain_state.is_some()
            && self.wallet.is_some()
            && self.redistribution.is_some()
    }
}

/// Snapshot fed to the S2 Stamps screen. `/stamps` polled at the
/// slower 10 s cadence per `docs/PLAN.md` § 9 — postage state is
/// updated on chain, not at request rate.
#[derive(Clone, Debug, Default)]
pub struct StampsSnapshot {
    pub batches: Vec<PostageBatch>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl StampsSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S3 SWAP / cheques screen. `/chequebook/*` and
/// `/settlements` are slow-changing — chain-rate at most — so the
/// poll cadence is 30 s per `docs/PLAN.md` § 9.
#[derive(Clone, Debug, Default)]
pub struct SwapSnapshot {
    pub chequebook: Option<ChequebookBalance>,
    pub settlements: Option<Settlements>,
    pub time_settlements: Option<Settlements>,
    /// Last received cheque per peer (from `/chequebook/cheque`).
    pub last_received: Vec<LastCheque>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl SwapSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Watch-channel hub. Owns one [`watch::Sender`] per resource group;
/// hands out clones of the receiver via `health()` / `stamps()` /
/// `swap()` etc.
#[derive(Clone, Debug)]
pub struct BeeWatch {
    health_rx: watch::Receiver<HealthSnapshot>,
    stamps_rx: watch::Receiver<StampsSnapshot>,
    swap_rx: watch::Receiver<SwapSnapshot>,
    cancel: CancellationToken,
}

impl BeeWatch {
    /// Spawn the polling tasks. The returned hub stays alive (and
    /// pollers keep running) until `shutdown()` is called or `cancel`
    /// is cancelled by the caller's parent.
    pub fn start(client: Arc<ApiClient>, parent_cancel: &CancellationToken) -> Self {
        let cancel = parent_cancel.child_token();
        let (health_tx, health_rx) = watch::channel(HealthSnapshot::default());
        spawn_health_poller(
            client.clone(),
            health_tx,
            cancel.clone(),
            Duration::from_secs(2),
        );
        let (stamps_tx, stamps_rx) = watch::channel(StampsSnapshot::default());
        spawn_stamps_poller(
            client.clone(),
            stamps_tx,
            cancel.clone(),
            Duration::from_secs(10),
        );
        let (swap_tx, swap_rx) = watch::channel(SwapSnapshot::default());
        spawn_swap_poller(client, swap_tx, cancel.clone(), Duration::from_secs(30));
        Self {
            health_rx,
            stamps_rx,
            swap_rx,
            cancel,
        }
    }

    /// Subscribe to the health snapshot stream. Cheap; cloning the
    /// receiver does not start a new poller.
    pub fn health(&self) -> watch::Receiver<HealthSnapshot> {
        self.health_rx.clone()
    }

    /// Subscribe to the stamps snapshot stream.
    pub fn stamps(&self) -> watch::Receiver<StampsSnapshot> {
        self.stamps_rx.clone()
    }

    /// Subscribe to the swap snapshot stream.
    pub fn swap(&self) -> watch::Receiver<SwapSnapshot> {
        self.swap_rx.clone()
    }

    /// Cancel every polling task this hub owns. Idempotent.
    pub fn shutdown(&self) {
        self.cancel.cancel();
    }
}

/// Poll `/status` + `/chainstate` + `/wallet` + `/redistributionstate`
/// every `interval` and broadcast a coherent [`HealthSnapshot`].
fn spawn_health_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<HealthSnapshot>,
    cancel: CancellationToken,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let snap = collect_health(&client).await;
                    if tx.send(snap).is_err() {
                        break; // no receivers; nobody cares anymore
                    }
                }
            }
        }
    });
}

/// Poll `/stamps` every `interval` and broadcast a fresh
/// [`StampsSnapshot`].
fn spawn_stamps_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<StampsSnapshot>,
    cancel: CancellationToken,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let snap = collect_stamps(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_stamps(client: &ApiClient) -> StampsSnapshot {
    match client.bee().postage().get_postage_batches().await {
        Ok(batches) => StampsSnapshot {
            batches,
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => StampsSnapshot {
            batches: Vec::new(),
            last_error: Some(format!("stamps: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll the four `/chequebook` + `/settlement` endpoints every
/// `interval` and broadcast a fresh [`SwapSnapshot`].
fn spawn_swap_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<SwapSnapshot>,
    cancel: CancellationToken,
    interval: Duration,
) {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(interval);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = tick.tick() => {
                    let snap = collect_swap(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_swap(client: &ApiClient) -> SwapSnapshot {
    let bee = client.bee();
    let chequebook = bee.debug().chequebook_balance().await;
    let settlements = bee.debug().settlements().await;
    let time_settlements = bee.debug().time_settlements().await;
    let last_received = bee.debug().last_cheques().await;

    let mut snap = SwapSnapshot {
        last_update: Some(Instant::now()),
        ..Default::default()
    };
    let mut errors: Vec<String> = Vec::new();
    match chequebook {
        Ok(c) => snap.chequebook = Some(c),
        Err(e) => errors.push(format!("chequebook: {e}")),
    }
    match settlements {
        Ok(s) => snap.settlements = Some(s),
        Err(e) => errors.push(format!("settlements: {e}")),
    }
    match time_settlements {
        Ok(s) => snap.time_settlements = Some(s),
        Err(e) => errors.push(format!("timesettlements: {e}")),
    }
    match last_received {
        Ok(v) => snap.last_received = v,
        Err(e) => errors.push(format!("cheques: {e}")),
    }
    if !errors.is_empty() {
        snap.last_error = Some(errors.join("; "));
    }
    snap
}

async fn collect_health(client: &ApiClient) -> HealthSnapshot {
    let bee = client.bee();

    // Time the cheap /health probe alongside the rest so the header
    // bar can show a single representative latency.
    let ping_start = Instant::now();
    let health_ok = bee.debug().health().await.is_ok();
    let last_ping = health_ok.then(|| ping_start.elapsed());

    let status = bee.debug().status().await;
    let chain_state = bee.debug().chain_state().await;
    let wallet = bee.debug().wallet().await;
    let redistribution = bee.debug().redistribution_state().await;

    let mut snap = HealthSnapshot {
        last_ping,
        last_update: Some(Instant::now()),
        ..Default::default()
    };
    let mut errors: Vec<String> = Vec::new();
    match status {
        Ok(s) => snap.status = Some(s),
        Err(e) => errors.push(format!("status: {e}")),
    }
    match chain_state {
        Ok(c) => snap.chain_state = Some(c),
        Err(e) => errors.push(format!("chainstate: {e}")),
    }
    match wallet {
        Ok(w) => snap.wallet = Some(w),
        Err(e) => errors.push(format!("wallet: {e}")),
    }
    match redistribution {
        Ok(r) => snap.redistribution = Some(r),
        Err(e) => errors.push(format!("redistributionstate: {e}")),
    }
    if !errors.is_empty() {
        snap.last_error = Some(errors.join("; "));
    }
    snap
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fully_loaded_default_is_false() {
        assert!(!HealthSnapshot::default().is_fully_loaded());
    }

    #[test]
    fn fully_loaded_requires_no_error_and_all_fields() {
        // ChainState and Wallet don't implement Default; build empty
        // instances via JSON to keep the test self-contained.
        let snap = HealthSnapshot {
            status: Some(Status::default()),
            chain_state: Some(serde_json::from_str(r#"{"block":0,"chainTip":0}"#).unwrap()),
            wallet: Some(
                serde_json::from_str(
                    r#"{"chainID":1,"walletAddress":"0x0000000000000000000000000000000000000000"}"#,
                )
                .unwrap(),
            ),
            redistribution: Some(RedistributionState::default()),
            ..Default::default()
        };
        assert!(snap.is_fully_loaded());
        let mut bad = snap;
        bad.last_error = Some("boom".into());
        assert!(!bad.is_fully_loaded());
    }
}
