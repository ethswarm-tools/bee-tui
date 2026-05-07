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

use bee::debug::{ChainState, RedistributionState, Status, Wallet};
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

/// Watch-channel hub. Owns one [`watch::Sender`] per resource group;
/// hands out clones of the receiver via `health()` etc.
#[derive(Clone, Debug)]
pub struct BeeWatch {
    health_rx: watch::Receiver<HealthSnapshot>,
    cancel: CancellationToken,
}

impl BeeWatch {
    /// Spawn the polling tasks. The returned hub stays alive (and
    /// pollers keep running) until `shutdown()` is called or `cancel`
    /// is cancelled by the caller's parent.
    pub fn start(client: Arc<ApiClient>, parent_cancel: &CancellationToken) -> Self {
        let cancel = parent_cancel.child_token();
        let (health_tx, health_rx) = watch::channel(HealthSnapshot::default());
        spawn_health_poller(client, health_tx, cancel.clone(), Duration::from_secs(2));
        Self { health_rx, cancel }
    }

    /// Subscribe to the health snapshot stream. Cheap; cloning the
    /// receiver does not start a new poller.
    pub fn health(&self) -> watch::Receiver<HealthSnapshot> {
        self.health_rx.clone()
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
