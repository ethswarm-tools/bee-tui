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

use bee::api::Tag;
use bee::debug::{
    Addresses, ChainState, ChequebookBalance, LastCheque, RedistributionState, Settlements, Status,
    Topology, TransactionInfo, Wallet,
};
use bee::postage::PostageBatch;
use bee::swarm::Reference;
use num_bigint::BigInt;
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
    /// On-chain chequebook contract address from
    /// `/chequebook/address`. Pasted onto the S3 header so operators
    /// can jump straight to a block explorer without unpacking the
    /// full `/wallet` response.
    pub chequebook_address: Option<String>,
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

/// Snapshot fed to the S9 Tags / uploads screen. `/tags` is polled
/// at 5 s — slow enough to be cheap on a quiet node, quick enough
/// that an in-progress upload's split / sent / synced columns visibly
/// tick. PLAN proposes 1 s when uploads are active; bumping the
/// cadence dynamically can land in a follow-up once we observe real
/// usage.
#[derive(Clone, Debug, Default)]
pub struct TagsSnapshot {
    pub tags: Vec<Tag>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl TagsSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S11 Pins screen. `/pins` is operator-driven
/// (it only changes when someone explicitly pins/unpins a reference)
/// so the poll cadence is the slow tier. Per-pin integrity isn't
/// auto-polled — `/pins/check` walks the entire chunk graph and is
/// expensive — operators trigger it on demand from the screen.
#[derive(Clone, Debug, Default)]
pub struct PinsSnapshot {
    /// References returned by `GET /pins`, in Bee's response order.
    pub pins: Vec<Reference>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl PinsSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S8 RPC / API health screen. `/transactions`
/// only changes when the operator submits something (postage topup,
/// stake deposit, withdrawal, etc.); 30 s cadence is the same tier
/// as SWAP and Lottery — slow enough to be cheap, quick enough that
/// a stuck pending TX shows up within a tick of submission.
#[derive(Clone, Debug, Default)]
pub struct TransactionsSnapshot {
    pub pending: Vec<TransactionInfo>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl TransactionsSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S7 Network/NAT screen. `/addresses` doesn't
/// change unless the node restarts, so the cadence is 60 s — slow
/// enough to be invisible in the command-log pane but quick enough
/// to catch a restart-induced overlay change.
#[derive(Clone, Debug, Default)]
pub struct NetworkSnapshot {
    pub addresses: Option<Addresses>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl NetworkSnapshot {
    pub fn is_loaded(&self) -> bool {
        self.addresses.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S6 Peers screen and the S1 bin-saturation
/// gate. `/topology` is polled at 5 s — per-bin populations don't
/// drift faster than peer churn, but the operator does want to see
/// "bin 4 starving" go yellow within a few ticks of the issue.
#[derive(Clone, Debug, Default)]
pub struct TopologySnapshot {
    pub topology: Option<Topology>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl TopologySnapshot {
    pub fn is_loaded(&self) -> bool {
        self.topology.is_some() && self.last_error.is_none()
    }
}

/// Snapshot fed to the S4 Lottery screen. `/stake` is operator-driven
/// (deposit / withdraw transactions only) so the cadence is 30 s per
/// `docs/PLAN.md` § 9 — same as SWAP. The redistribution-state half of
/// the screen is read off the existing 2 s [`HealthSnapshot`] feed; the
/// Lottery component subscribes to both.
#[derive(Clone, Debug, Default)]
pub struct LotterySnapshot {
    /// `/stake` — currently staked BZZ (PLUR).
    pub staked: Option<BigInt>,
    pub last_error: Option<String>,
    pub last_update: Option<Instant>,
}

impl LotterySnapshot {
    pub fn is_loaded(&self) -> bool {
        self.last_update.is_some() && self.last_error.is_none()
    }
}

/// Polling-cadence preset chosen by `[ui].refresh` in `config.toml`.
/// Each variant maps every per-resource interval to a fixed value,
/// trading freshness for HTTP volume.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshProfile {
    /// Original cockpit cadence — 2 s health / 5 s topology+tags /
    /// 30 s swap+lottery+transactions / 60 s network. Use when
    /// actively diagnosing; floods the bottom Bee HTTP tab.
    Live,
    /// Halved-fast-tier cadence (4 s health, 10 s topology+tags,
    /// otherwise the same as Live). Default for new installs since
    /// the tabbed log pane shipped — keeps the Bee HTTP tab readable
    /// without losing operator-relevant freshness.
    Default,
    /// Minimal-traffic cadence (8 s health, 20 s topology+tags,
    /// 60 s mid tier, 120 s network). For "leave it open all day"
    /// monitoring.
    Slow,
}

impl RefreshProfile {
    /// Parse from the `[ui].refresh` config value. Unknown strings
    /// fall back to `Default` with a tracing warning so a typo
    /// can't break startup.
    pub fn from_config(s: &str) -> Self {
        match s {
            "live" => Self::Live,
            "default" => Self::Default,
            "slow" => Self::Slow,
            other => {
                tracing::warn!(
                    "unknown [ui].refresh value {other:?}; falling back to \"default\". \
                     Recognised: live | default | slow."
                );
                Self::Default
            }
        }
    }

    pub fn health(self) -> Duration {
        match self {
            Self::Live => Duration::from_secs(2),
            Self::Default => Duration::from_secs(4),
            Self::Slow => Duration::from_secs(8),
        }
    }
    pub fn topology(self) -> Duration {
        match self {
            Self::Live => Duration::from_secs(5),
            Self::Default => Duration::from_secs(10),
            Self::Slow => Duration::from_secs(20),
        }
    }
    pub fn stamps(self) -> Duration {
        // Stamps were already 10s under Live; we slow them under
        // Slow only. Utilization grows at upload rate, not poll rate.
        match self {
            Self::Live | Self::Default => Duration::from_secs(10),
            Self::Slow => Duration::from_secs(20),
        }
    }
    pub fn tags(self) -> Duration {
        match self {
            Self::Live => Duration::from_secs(5),
            Self::Default => Duration::from_secs(10),
            Self::Slow => Duration::from_secs(20),
        }
    }
    pub fn swap(self) -> Duration {
        match self {
            Self::Live | Self::Default => Duration::from_secs(30),
            Self::Slow => Duration::from_secs(60),
        }
    }
    pub fn lottery(self) -> Duration {
        match self {
            Self::Live | Self::Default => Duration::from_secs(30),
            Self::Slow => Duration::from_secs(60),
        }
    }
    pub fn transactions(self) -> Duration {
        match self {
            Self::Live | Self::Default => Duration::from_secs(30),
            Self::Slow => Duration::from_secs(60),
        }
    }
    pub fn network(self) -> Duration {
        match self {
            Self::Live | Self::Default => Duration::from_secs(60),
            Self::Slow => Duration::from_secs(120),
        }
    }
    pub fn pins(self) -> Duration {
        // /pins is a tiny list; cadence is set by how fast a pin set
        // can change in practice (operator-driven). Same tier as
        // swap/lottery/transactions.
        match self {
            Self::Live | Self::Default => Duration::from_secs(30),
            Self::Slow => Duration::from_secs(60),
        }
    }
}

/// Watch-channel hub. Owns one [`watch::Sender`] per resource group;
/// hands out clones of the receiver via `health()` / `stamps()` /
/// `swap()` / `lottery()` / `topology()` / `network()` etc.
#[derive(Clone, Debug)]
pub struct BeeWatch {
    health_rx: watch::Receiver<HealthSnapshot>,
    stamps_rx: watch::Receiver<StampsSnapshot>,
    swap_rx: watch::Receiver<SwapSnapshot>,
    lottery_rx: watch::Receiver<LotterySnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    network_rx: watch::Receiver<NetworkSnapshot>,
    transactions_rx: watch::Receiver<TransactionsSnapshot>,
    tags_rx: watch::Receiver<TagsSnapshot>,
    pins_rx: watch::Receiver<PinsSnapshot>,
    cancel: CancellationToken,
}

impl BeeWatch {
    /// Spawn the polling tasks at [`RefreshProfile::Default`] cadence.
    /// Use [`BeeWatch::start_with_profile`] to override.
    pub fn start(client: Arc<ApiClient>, parent_cancel: &CancellationToken) -> Self {
        Self::start_with_profile(client, parent_cancel, RefreshProfile::Default)
    }

    /// Spawn the polling tasks at the supplied cadence. The returned
    /// hub stays alive (and pollers keep running) until `shutdown()`
    /// is called or `cancel` is cancelled by the caller's parent.
    pub fn start_with_profile(
        client: Arc<ApiClient>,
        parent_cancel: &CancellationToken,
        profile: RefreshProfile,
    ) -> Self {
        let cancel = parent_cancel.child_token();
        let (health_tx, health_rx) = watch::channel(HealthSnapshot::default());
        spawn_health_poller(client.clone(), health_tx, cancel.clone(), profile.health());
        let (stamps_tx, stamps_rx) = watch::channel(StampsSnapshot::default());
        spawn_stamps_poller(client.clone(), stamps_tx, cancel.clone(), profile.stamps());
        let (swap_tx, swap_rx) = watch::channel(SwapSnapshot::default());
        spawn_swap_poller(client.clone(), swap_tx, cancel.clone(), profile.swap());
        let (lottery_tx, lottery_rx) = watch::channel(LotterySnapshot::default());
        spawn_lottery_poller(
            client.clone(),
            lottery_tx,
            cancel.clone(),
            profile.lottery(),
        );
        let (topology_tx, topology_rx) = watch::channel(TopologySnapshot::default());
        spawn_topology_poller(
            client.clone(),
            topology_tx,
            cancel.clone(),
            profile.topology(),
        );
        let (network_tx, network_rx) = watch::channel(NetworkSnapshot::default());
        spawn_network_poller(
            client.clone(),
            network_tx,
            cancel.clone(),
            profile.network(),
        );
        let (transactions_tx, transactions_rx) = watch::channel(TransactionsSnapshot::default());
        spawn_transactions_poller(
            client.clone(),
            transactions_tx,
            cancel.clone(),
            profile.transactions(),
        );
        let (tags_tx, tags_rx) = watch::channel(TagsSnapshot::default());
        spawn_tags_poller(client.clone(), tags_tx, cancel.clone(), profile.tags());
        let (pins_tx, pins_rx) = watch::channel(PinsSnapshot::default());
        spawn_pins_poller(client, pins_tx, cancel.clone(), profile.pins());
        Self {
            health_rx,
            stamps_rx,
            swap_rx,
            lottery_rx,
            topology_rx,
            network_rx,
            transactions_rx,
            tags_rx,
            pins_rx,
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

    /// Subscribe to the lottery snapshot stream (`/stake`).
    pub fn lottery(&self) -> watch::Receiver<LotterySnapshot> {
        self.lottery_rx.clone()
    }

    /// Subscribe to the topology snapshot stream (`/topology`).
    pub fn topology(&self) -> watch::Receiver<TopologySnapshot> {
        self.topology_rx.clone()
    }

    /// Subscribe to the network snapshot stream (`/addresses`).
    pub fn network(&self) -> watch::Receiver<NetworkSnapshot> {
        self.network_rx.clone()
    }

    /// Subscribe to the pending-transactions snapshot stream
    /// (`/transactions`).
    pub fn transactions(&self) -> watch::Receiver<TransactionsSnapshot> {
        self.transactions_rx.clone()
    }

    /// Subscribe to the tags snapshot stream (`/tags`).
    pub fn tags(&self) -> watch::Receiver<TagsSnapshot> {
        self.tags_rx.clone()
    }

    /// Subscribe to the pins snapshot stream (`/pins`). Per-pin
    /// integrity (from `/pins/check`) isn't part of this stream —
    /// the S11 component walks integrity on demand because the
    /// underlying check is expensive.
    pub fn pins(&self) -> watch::Receiver<PinsSnapshot> {
        self.pins_rx.clone()
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
    let chequebook_address = bee.debug().chequebook_address().await;
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
    // Address-fetch failure is non-fatal — surfacing the contract
    // address is a "nice to have" header decoration; the rest of the
    // SWAP screen keeps working without it.
    if let Ok(a) = chequebook_address {
        snap.chequebook_address = Some(a);
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

/// Poll `/stake` every `interval` and broadcast a fresh
/// [`LotterySnapshot`].
fn spawn_lottery_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<LotterySnapshot>,
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
                    let snap = collect_lottery(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_lottery(client: &ApiClient) -> LotterySnapshot {
    match client.bee().debug().stake().await {
        Ok(staked) => LotterySnapshot {
            staked: Some(staked),
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => LotterySnapshot {
            staked: None,
            last_error: Some(format!("stake: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll `/topology` every `interval` and broadcast a fresh
/// [`TopologySnapshot`].
fn spawn_topology_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<TopologySnapshot>,
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
                    let snap = collect_topology(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_topology(client: &ApiClient) -> TopologySnapshot {
    match client.bee().debug().topology().await {
        Ok(topology) => TopologySnapshot {
            topology: Some(topology),
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => TopologySnapshot {
            topology: None,
            last_error: Some(format!("topology: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll `/addresses` every `interval` and broadcast a fresh
/// [`NetworkSnapshot`].
fn spawn_network_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<NetworkSnapshot>,
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
                    let snap = collect_network(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_network(client: &ApiClient) -> NetworkSnapshot {
    match client.bee().debug().addresses().await {
        Ok(addresses) => NetworkSnapshot {
            addresses: Some(addresses),
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => NetworkSnapshot {
            addresses: None,
            last_error: Some(format!("addresses: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll `/transactions` every `interval` and broadcast a fresh
/// [`TransactionsSnapshot`].
fn spawn_transactions_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<TransactionsSnapshot>,
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
                    let snap = collect_transactions(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_transactions(client: &ApiClient) -> TransactionsSnapshot {
    match client.bee().debug().pending_transactions().await {
        Ok(pending) => TransactionsSnapshot {
            pending,
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => TransactionsSnapshot {
            pending: Vec::new(),
            last_error: Some(format!("transactions: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll `/tags` every `interval` and broadcast a fresh
/// [`TagsSnapshot`].
fn spawn_tags_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<TagsSnapshot>,
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
                    let snap = collect_tags(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_tags(client: &ApiClient) -> TagsSnapshot {
    match client.bee().api().list_tags(None, None).await {
        Ok(tags) => TagsSnapshot {
            tags,
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => TagsSnapshot {
            tags: Vec::new(),
            last_error: Some(format!("tags: {e}")),
            last_update: Some(Instant::now()),
        },
    }
}

/// Poll `GET /pins` and broadcast a fresh [`PinsSnapshot`]. Same
/// shape as the other slow-tier pollers; integrity (`/pins/check`)
/// is *not* called from here — operators trigger it from the S11
/// component on demand.
fn spawn_pins_poller(
    client: Arc<ApiClient>,
    tx: watch::Sender<PinsSnapshot>,
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
                    let snap = collect_pins(&client).await;
                    if tx.send(snap).is_err() {
                        break;
                    }
                }
            }
        }
    });
}

async fn collect_pins(client: &ApiClient) -> PinsSnapshot {
    match client.bee().api().list_pins().await {
        Ok(pins) => PinsSnapshot {
            pins,
            last_error: None,
            last_update: Some(Instant::now()),
        },
        Err(e) => PinsSnapshot {
            pins: Vec::new(),
            last_error: Some(format!("pins: {e}")),
            last_update: Some(Instant::now()),
        },
    }
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
