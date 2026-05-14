use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::eyre;
use crossterm::event::KeyEvent;
use ratatui::prelude::Rect;
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, watch};
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{
    action::Action,
    api::ApiClient,
    bee_log_discover::{self, BeeLogSource, DiscoveryResult},
    bee_supervisor::{BeeStatus, BeeSupervisor},
    components::{
        Component,
        api_health::ApiHealth,
        feed_timeline::FeedTimeline,
        health::{Gate, GateStatus, Health},
        log_pane::{BeeLogLine, LogPane, LogTab},
        lottery::Lottery,
        manifest::Manifest,
        network::Network,
        peers::Peers,
        pins::Pins,
        pubsub::Pubsub,
        stamps::Stamps,
        swap::Swap,
        tags::Tags,
        warmup::Warmup,
        watchlist::Watchlist,
    },
    config::Config,
    config_doctor, durability, economics_oracle, log_capture,
    manifest_walker::{self, InspectResult},
    pprof_bundle, stamp_preview,
    state::State,
    theme,
    tui::{Event, Tui},
    utility_verbs, version_check,
    watch::{BeeWatch, HealthSnapshot, RefreshProfile},
};

pub struct App {
    config: Config,
    tick_rate: f64,
    frame_rate: f64,
    /// Top-level screens, in display order. Tab cycles among them.
    /// v0.4 also wires the k9s-style `:command` switcher so users
    /// can jump directly with `:peers`, `:stamps`, etc.
    screens: Vec<Box<dyn Component>>,
    /// Index into [`Self::screens`] for the currently visible screen.
    current_screen: usize,
    /// Always-on bottom strip; not part of `screens` because it
    /// renders alongside whatever screen is active. Tabbed across
    /// Errors/Warn/Info/Debug/BeeHttp/SelfHttp.
    log_pane: LogPane,
    /// Where the persisted UI state (tab + height) lives on disk.
    /// Computed once at startup; rewritten on quit.
    state_path: PathBuf,
    should_quit: bool,
    should_suspend: bool,
    mode: Mode,
    last_tick_key_events: Vec<KeyEvent>,
    action_tx: mpsc::UnboundedSender<Action>,
    action_rx: mpsc::UnboundedReceiver<Action>,
    /// Root cancellation token. Children: BeeWatch hub → per-resource
    /// pollers. Cancelling this on quit unwinds every spawned task.
    root_cancel: CancellationToken,
    /// Active Bee node connection; cheap to clone (`Arc<Inner>` under
    /// the hood). Read by future header bar + multi-node switcher.
    #[allow(dead_code)]
    api: Arc<ApiClient>,
    /// Watch / informer hub feeding screens.
    watch: BeeWatch,
    /// Top-bar reuses the health snapshot for the live ping
    /// indicator. Cheap clone of the watch receiver.
    health_rx: watch::Receiver<HealthSnapshot>,
    /// `Some(buf)` while the user is typing a `:command`. The
    /// buffer holds the characters typed *after* the leading colon.
    command_buffer: Option<String>,
    /// Index into the *filtered* command-suggestion list of the row
    /// currently highlighted by the Up/Down keys. Reset to 0 on every
    /// buffer mutation so a fresh prefix always starts at the top
    /// match.
    command_suggestion_index: usize,
    /// Status / error from the most recent `:command`, persisted on
    /// the command-bar line until the user enters command mode again.
    /// Cleared when `command_buffer` transitions to `Some`.
    command_status: Option<CommandStatus>,
    /// `true` while the `?` help overlay is up. Renders on top of
    /// the active screen; `?` toggles, `Esc` dismisses.
    help_visible: bool,
    /// Tracks the moment the operator pressed `q` once. A second
    /// `q` within [`QUIT_CONFIRM_WINDOW`] commits the quit; otherwise
    /// it expires and the cockpit keeps running. Prevents a single
    /// stray keystroke from killing a session the operator is
    /// actively monitoring.
    quit_pending: Option<Instant>,
    /// `Some` when the `[bee]` block (or `--bee-bin` / `--bee-config`)
    /// is configured and we're acting as Bee's parent process. `None`
    /// for the legacy "connect to a running Bee" flow.
    supervisor: Option<BeeSupervisor>,
    /// Last-observed status of the supervised Bee child. Refreshed
    /// each Tick from `supervisor.status()`. Surfaced in the top bar
    /// so a mid-session crash is visible to the operator (variant B
    /// of the crash-handling spec — show, don't auto-restart).
    bee_status: BeeStatus,
    /// Receiver paired with the bee-log tailer task. `None` when
    /// there is no log source — not the supervisor, and the active
    /// node has no `log_file`. Drained on each Tick into the LogPane.
    bee_log_rx: Option<mpsc::UnboundedReceiver<(LogTab, BeeLogLine)>>,
    /// Cancellation handle for the *external* bee-log tailer (the
    /// one following a configured `[[nodes]].log_file`). `Some` only
    /// in external-tail mode; `switch_context` cancels it and spawns
    /// a fresh tailer for the new node. The supervisor's own tailer
    /// is not tracked here — it lives for the whole session under
    /// `root_cancel`.
    bee_log_tailer_cancel: Option<CancellationToken>,
    /// Channel for async-completing `:command` results. Verbs that
    /// can't return their answer synchronously (e.g. `:probe-upload`
    /// which has to wait on an HTTP round-trip) hand a clone of the
    /// sender to a tokio task and surface the outcome on completion;
    /// the App drains this on every Tick into `command_status`.
    cmd_status_tx: mpsc::UnboundedSender<CommandStatus>,
    cmd_status_rx: mpsc::UnboundedReceiver<CommandStatus>,
    /// Async-result channel for durability-check completions. Each
    /// result is forwarded to the S13 Watchlist screen on the next
    /// Tick. Sibling to `cmd_status_tx` rather than overloading it
    /// because the Watchlist row carries structured data, not a
    /// formatted `CommandStatus` string.
    durability_tx: mpsc::UnboundedSender<crate::durability::DurabilityResult>,
    durability_rx: mpsc::UnboundedReceiver<crate::durability::DurabilityResult>,
    /// Async-result channel for `:feed-timeline` walks. Each
    /// completed walk arrives as a `FeedTimelineMessage` and is
    /// forwarded to the S14 screen on the next Tick.
    feed_timeline_tx: mpsc::UnboundedSender<FeedTimelineMessage>,
    feed_timeline_rx: mpsc::UnboundedReceiver<FeedTimelineMessage>,
    /// Active `:watch-ref` daemon loops keyed by reference hex. Each
    /// entry owns a `CancellationToken` whose `cancel()` stops the
    /// daemon's tokio task on the next iteration boundary.
    watch_refs: std::collections::HashMap<String, CancellationToken>,
    /// Active pubsub subscriptions (PSS / GSOC) keyed by sub-id.
    /// Each entry's `CancellationToken` stops both the websocket
    /// recv loop and the forwarding task that pushes messages onto
    /// `pubsub_msg_tx`.
    pubsub_subs: std::collections::HashMap<String, CancellationToken>,
    /// Optional shared history-file writer. `Some(...)` when
    /// `[pubsub].history_file` is configured; cloned into each
    /// watcher so JSONL appends serialise across subscriptions.
    pubsub_history: crate::pubsub::HistoryWriter,
    /// Async-message channel feeding the S15 Pubsub screen with
    /// every PSS / GSOC frame the active subscriptions deliver.
    pubsub_msg_tx: mpsc::UnboundedSender<crate::pubsub::PubsubMessage>,
    pubsub_msg_rx: mpsc::UnboundedReceiver<crate::pubsub::PubsubMessage>,
    /// Per-gate transition tracker for the optional webhook alerter.
    /// On every Tick we feed it the latest gates; it returns the
    /// transitions worth pinging on (debounced per-gate). When
    /// `[alerts].webhook_url` is unset, [`Self::tick_alerts`] short-
    /// circuits before touching this state so the cost is one
    /// `Option::is_none` check per Tick.
    alert_state: crate::alerts::AlertState,
    /// True while the `Ctrl-N` (or `:nodes`) node-picker overlay is
    /// visible. Key handling routes to picker-only bindings (↑/↓ /
    /// Enter / Esc) when set; the cockpit beneath keeps rendering.
    nodes_picker_visible: bool,
    /// Cursor row in the node-picker overlay, indexed into
    /// `self.config.nodes`. Clamped on every render so config edits
    /// that shrink the list can't leave it pointing past the end.
    nodes_picker_selected: usize,
    /// Which page of the help overlay is showing. Tab cycles between
    /// Keys (default) and Verbs.
    help_page: HelpPage,
    /// Watch receiver feeding the S15 Fleet screen. Cloned into each
    /// fresh `Fleet` component built by `build_screens`; the poller
    /// itself lives across context switches (it polls **every**
    /// `[[nodes]]` entry, not just the active one), so we hold the
    /// rx on App rather than re-creating it on `switch_context`.
    fleet_rx: watch::Receiver<crate::fleet::FleetSnapshot>,
    /// Operator-trigger channel for the fleet poller's "re-poll now"
    /// signal (S15 row `r` key). Same lifetime as `fleet_rx`.
    fleet_resync_tx: tokio::sync::mpsc::UnboundedSender<()>,
    /// Batch-economics modal state. Opened with `E` from anywhere
    /// (anchored conceptually to S3 SWAP / S2 Stamps but the modal
    /// floats over whichever screen is active). Drives `:topup-preview`
    /// / `:dilute-preview` / `:extend-preview` / `:buy-preview` /
    /// `:plan-batch` through a guided form, so operators don't have
    /// to remember the arg order.
    batch_modal: BatchModal,
    /// Auto-restart watchdog state. `None` when bee-tui isn't acting
    /// as Bee's supervisor or `[bee.supervisor].auto_restart = false`.
    /// `Some` otherwise — tracks restart count + history (sliding
    /// one-hour window for budget enforcement), the spawn parameters
    /// to re-issue, and the next-allowed-restart timestamp.
    supervisor_watchdog: Option<SupervisorWatchdog>,
    /// Fleet-aggregate webhook state. Tracks per-node previous
    /// status (so we only fire on transitions, not steady-state
    /// failures) and the buffered pending alerts within the current
    /// coalesce window. Active only when
    /// `[fleet].aggregate_webhook_url` is set; otherwise the
    /// struct's fields stay empty and `tick_fleet_aggregate` is a
    /// cheap no-op.
    fleet_aggregator: FleetAggregator,
    /// True when `Shift+L` has expanded the log pane to fill the
    /// middle of the cockpit. The current screen body is hidden
    /// while this is on. Toggled by the same key — same data,
    /// same tabs, same filter; just bigger.
    log_fullscreen: bool,
    /// In-cockpit notification center. Ingests every alert the
    /// existing `tick_alerts` and `tick_fleet_aggregate` produce,
    /// surfaces them as top-right toasts, persists them in a 200-
    /// entry ring buffer for the history overlay, and optionally
    /// escalates to OS notifications / terminal bell per
    /// `[notifications]` config.
    notifications: crate::notifications::NotificationCenter,
    /// True while the `Ctrl+Alt+N` (or `:notifications`) history
    /// overlay is visible. Same modal discipline as help / picker /
    /// batch modal — keystrokes other than Esc are swallowed.
    notifications_overlay_visible: bool,
}

/// Per-node previous status + the buffered pending alerts. The
/// aggregator looks at the latest `FleetSnapshot` on every tick,
/// notes any nodes that flipped to a worse status, buffers them,
/// and fires one webhook per `[fleet].aggregate_window_secs`.
#[derive(Debug, Default, Clone)]
pub struct FleetAggregator {
    /// Last seen status per node name. Used to detect transitions
    /// (and to silence steady-state "still failing" noise).
    pub last_status: std::collections::HashMap<String, crate::fleet::FleetStatus>,
    /// Buffered alerts awaiting consolidation. Each entry: `(node,
    /// from→to, why)`. Drained when the window elapses and we POST.
    pub pending: Vec<FleetAlertEntry>,
    /// Wall-clock when the current window opened. `None` means
    /// "no pending alerts yet" — a transition will both buffer and
    /// arm the window.
    pub window_opened_at: Option<Instant>,
    /// Most recent successful fire timestamp. Surfaced for tests +
    /// future "last alert N min ago" UI.
    pub last_fired_at: Option<Instant>,
}

#[derive(Debug, Clone)]
pub struct FleetAlertEntry {
    pub node: String,
    pub from: crate::fleet::FleetStatus,
    pub to: crate::fleet::FleetStatus,
    pub why: Option<String>,
}

impl FleetAggregator {
    /// Compare each row's current status to the recorded previous
    /// and append any "worth alerting" transitions to `pending`.
    /// Worth-alerting rules:
    /// - Pass / Unknown → Warn / Fail (degradation)
    /// - Warn → Fail (escalation)
    /// - Fail → Pass / Warn (recovery; useful to know "we're back")
    ///
    /// Returns the number of new entries buffered.
    pub fn ingest_snapshot(
        &mut self,
        snapshot: &crate::fleet::FleetSnapshot,
        now: Instant,
    ) -> usize {
        use crate::fleet::FleetStatus;
        let mut added = 0;
        for row in &snapshot.rows {
            let prev = self
                .last_status
                .get(&row.name)
                .copied()
                .unwrap_or(FleetStatus::Unknown);
            // Persist the latest status regardless of whether we
            // emit an alert — that's the comparison baseline for
            // the next tick.
            self.last_status.insert(row.name.clone(), row.status);
            // Skip Unknown → anything (or anything → Unknown);
            // those are cold-start / probe-loss transitions, not
            // node-state changes.
            if prev == FleetStatus::Unknown || row.status == FleetStatus::Unknown {
                continue;
            }
            if prev == row.status {
                continue;
            }
            // Worth-alerting filter:
            let interesting = matches!(
                (prev, row.status),
                (FleetStatus::Pass, FleetStatus::Warn | FleetStatus::Fail,)
                    | (FleetStatus::Warn, FleetStatus::Fail)
                    | (FleetStatus::Fail, FleetStatus::Pass | FleetStatus::Warn)
                    | (FleetStatus::Warn, FleetStatus::Pass)
            );
            if !interesting {
                continue;
            }
            self.pending.push(FleetAlertEntry {
                node: row.name.clone(),
                from: prev,
                to: row.status,
                why: row.why.clone(),
            });
            added += 1;
            if self.window_opened_at.is_none() {
                self.window_opened_at = Some(now);
            }
        }
        added
    }

    /// Should we fire the consolidated webhook now? Pure decision
    /// driven by the window timer. Returns the drained pending list
    /// (caller spawns the POST) and clears the window.
    pub fn drain_if_window_elapsed(
        &mut self,
        now: Instant,
        window: Duration,
    ) -> Option<Vec<FleetAlertEntry>> {
        let opened = self.window_opened_at?;
        if now.duration_since(opened) < window {
            return None;
        }
        if self.pending.is_empty() {
            self.window_opened_at = None;
            return None;
        }
        let drained = std::mem::take(&mut self.pending);
        self.window_opened_at = None;
        self.last_fired_at = Some(now);
        Some(drained)
    }

    /// Build the human-readable message body. Pure — assertion-friendly.
    pub fn format_message(entries: &[FleetAlertEntry]) -> String {
        if entries.is_empty() {
            return String::new();
        }
        let mut lines = Vec::with_capacity(entries.len() + 1);
        let fail_count = entries
            .iter()
            .filter(|e| e.to == crate::fleet::FleetStatus::Fail)
            .count();
        let warn_count = entries
            .iter()
            .filter(|e| e.to == crate::fleet::FleetStatus::Warn)
            .count();
        let recovered_count = entries
            .iter()
            .filter(|e| {
                e.to == crate::fleet::FleetStatus::Pass
                    && (e.from == crate::fleet::FleetStatus::Fail
                        || e.from == crate::fleet::FleetStatus::Warn)
            })
            .count();
        let mut headline_parts = Vec::new();
        if fail_count > 0 {
            headline_parts.push(format!("{fail_count} fail"));
        }
        if warn_count > 0 {
            headline_parts.push(format!("{warn_count} warn"));
        }
        if recovered_count > 0 {
            headline_parts.push(format!("{recovered_count} recovered"));
        }
        lines.push(format!("Fleet alert: {}", headline_parts.join(" · ")));
        for e in entries {
            let arrow = format!("{:?} → {:?}", e.from, e.to);
            if let Some(why) = &e.why {
                lines.push(format!("• {}: {arrow} ({why})", e.node));
            } else {
                lines.push(format!("• {}: {arrow}", e.node));
            }
        }
        lines.join("\n")
    }
}

/// Auto-restart state for the supervised Bee child. Pure state +
/// pure decision functions; the actual restart-spawn is driven by
/// `App::tick_supervisor_watchdog` so the I/O stays in `app.rs`.
#[derive(Debug, Clone)]
pub struct SupervisorWatchdog {
    /// `[bee].bin` — kept so we can re-spawn on crash.
    pub bin: std::path::PathBuf,
    /// `[bee].config` — kept for the same reason.
    pub config: std::path::PathBuf,
    /// `[bee.logs]` snapshot, threaded through to each restart's
    /// `BeeSupervisor::spawn` call.
    pub logs: crate::config::BeeLogsConfig,
    /// `[bee.supervisor]` policy.
    pub policy: crate::config::BeeSupervisorConfig,
    /// Wall-clock timestamps of every restart attempt. Pruned to
    /// the last hour on every check so the sliding-window budget is
    /// O(1) per tick on a sane fleet.
    pub restart_history: std::collections::VecDeque<Instant>,
    /// Earliest wall-clock at which the next restart may be
    /// attempted. `None` means "no pending wait" — the watchdog
    /// will try the next non-Running tick.
    pub next_attempt_at: Option<Instant>,
    /// Cumulative restart count across the session. Surfaced in the
    /// top bar chip (`bee: running 4d3h (2 restarts)`). Doesn't
    /// reset when `restart_history` slides; this is operator-facing.
    pub restart_count: u32,
}

impl SupervisorWatchdog {
    /// Number of restarts within the last hour. Pruning is done
    /// here so callers don't have to remember; the structure stays
    /// bounded.
    pub fn restarts_last_hour(&mut self, now: Instant) -> u32 {
        let cutoff = now.checked_sub(Duration::from_secs(3600));
        if let Some(c) = cutoff {
            while self
                .restart_history
                .front()
                .map(|t| *t < c)
                .unwrap_or(false)
            {
                self.restart_history.pop_front();
            }
        }
        self.restart_history.len() as u32
    }

    /// Backoff for the *next* restart attempt: `initial * 2^count`
    /// capped at `backoff_max_secs`. Pure — separated for testability.
    pub fn backoff_for(&self, attempt_idx: u32) -> Duration {
        let shift = attempt_idx.min(16); // 2^16 saturates fast
        let secs = self
            .policy
            .backoff_initial_secs
            .saturating_mul(1u64 << shift)
            .min(self.policy.backoff_max_secs);
        Duration::from_secs(secs.max(self.policy.backoff_initial_secs))
    }

    /// Should we attempt a restart right now? Pure decision over
    /// the watchdog state — separated so it can be unit-tested
    /// against fixture clocks.
    pub fn should_attempt(&mut self, now: Instant) -> bool {
        if !self.policy.auto_restart {
            return false;
        }
        if let Some(wait_until) = self.next_attempt_at {
            if now < wait_until {
                return false;
            }
        }
        let used = self.restarts_last_hour(now);
        used < self.policy.max_restarts_per_hour
    }

    /// Record that a restart was attempted at `now`. Adds the
    /// timestamp to history, increments the cumulative count, and
    /// computes the next-allowed-restart time using the exponential
    /// backoff curve.
    pub fn record_attempt(&mut self, now: Instant) {
        self.restart_history.push_back(now);
        self.restart_count = self.restart_count.saturating_add(1);
        let backoff = self.backoff_for(self.restart_count);
        self.next_attempt_at = Some(now + backoff);
    }

    /// Human-readable status line for the top bar.
    /// `bee running 4d3h (2 restarts)` / `bee: max restarts hit`.
    pub fn top_bar_label(&self, running: bool, uptime: Option<Duration>) -> String {
        if running {
            let up = uptime
                .map(format_duration_short)
                .unwrap_or_else(|| "?".into());
            if self.restart_count == 0 {
                format!("bee running {up}")
            } else {
                format!(
                    "bee running {up} ({} restart{})",
                    self.restart_count,
                    if self.restart_count == 1 { "" } else { "s" }
                )
            }
        } else if self.restart_history.len() as u32 >= self.policy.max_restarts_per_hour {
            format!(
                "bee: max restarts ({}/{}) hit",
                self.restart_history.len(),
                self.policy.max_restarts_per_hour
            )
        } else {
            "bee: restarting…".into()
        }
    }
}

/// Format a duration as a compact 2-unit label: `4d3h`, `12h5m`,
/// `8m30s`. Used by the supervisor's top-bar chip.
pub fn format_duration_short(d: Duration) -> String {
    let secs = d.as_secs();
    let days = secs / 86_400;
    let hours = (secs % 86_400) / 3_600;
    let mins = (secs % 3_600) / 60;
    let s = secs % 60;
    if days > 0 {
        format!("{days}d{hours}h")
    } else if hours > 0 {
        format!("{hours}h{mins}m")
    } else if mins > 0 {
        format!("{mins}m{s}s")
    } else {
        format!("{s}s")
    }
}

/// State of the S3 batch-economics modal. Walks the operator through
/// action selection → field entry → preview output, then dismisses
/// on Enter/Esc. Pure-state — the actual previews run through the
/// existing `run_*_preview` methods on App so there's no code
/// duplication.
#[derive(Debug, Default, Clone)]
pub struct BatchModal {
    pub visible: bool,
    pub action: Option<BatchAction>,
    /// One entry per committed field. `field_inputs.len()` == number
    /// of fields the operator has confirmed; the active field is at
    /// index `field_inputs.len()`.
    pub field_inputs: Vec<String>,
    /// Buffer for the currently-being-typed field. Committed into
    /// `field_inputs` on Enter.
    pub input_buffer: String,
    /// Preview output once all fields are submitted. `None` while
    /// the form is still being filled.
    pub result: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BatchAction {
    Topup,
    Dilute,
    Extend,
    Buy,
    Plan,
}

impl BatchAction {
    pub fn verb(self) -> &'static str {
        match self {
            BatchAction::Topup => "topup-preview",
            BatchAction::Dilute => "dilute-preview",
            BatchAction::Extend => "extend-preview",
            BatchAction::Buy => "buy-preview",
            BatchAction::Plan => "plan-batch",
        }
    }

    pub fn fields(self) -> &'static [&'static str] {
        match self {
            BatchAction::Topup => &["batch-prefix", "amount-PLUR-per-chunk"],
            BatchAction::Dilute => &["batch-prefix", "new-depth"],
            BatchAction::Extend => &["batch-prefix", "duration (e.g. 30d)"],
            BatchAction::Buy => &["depth", "amount-PLUR-per-chunk"],
            BatchAction::Plan => &["batch-prefix"],
        }
    }

    pub fn from_char(c: char) -> Option<Self> {
        match c.to_ascii_lowercase() {
            't' => Some(BatchAction::Topup),
            'd' => Some(BatchAction::Dilute),
            'e' => Some(BatchAction::Extend),
            'b' => Some(BatchAction::Buy),
            'p' => Some(BatchAction::Plan),
            _ => None,
        }
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelpPage {
    #[default]
    Keys,
    Verbs,
}

/// Window during which a second `q` press is interpreted as confirming
/// the quit. After this elapses the first press is forgotten.
const QUIT_CONFIRM_WINDOW: Duration = Duration::from_millis(1500);

/// Outcome from the most recently executed `:command`. Drives the
/// colour of the command-bar line in normal mode.
#[derive(Debug, Clone)]
pub enum CommandStatus {
    Info(String),
    Err(String),
}

/// Result variants that flow from `:feed-timeline`'s background
/// walk into the S14 screen. Drained by the Tick handler the same
/// way `cmd_status_rx` and `durability_rx` are.
#[derive(Debug, Clone)]
pub enum FeedTimelineMessage {
    Loaded(crate::feed_timeline::Timeline),
    Failed(String),
}

/// Names the top-level screens. Index matches position in
/// [`App::screens`].
const SCREEN_NAMES: &[&str] = &[
    "Health",
    "Stamps",
    "Swap",
    "Lottery",
    "Peers",
    "Network",
    "Warmup",
    "API",
    "Tags",
    "Pins",
    "Manifest",
    "Watchlist",
    "FeedTimeline",
    "Pubsub",
    "Fleet",
];

/// Catalog of every `:command` verb with a short description. Drives
/// the suggestion popup that surfaces matches as the operator types
/// (so they don't have to memorize the whole list). Aliases stay
/// implicit — they still work when typed but only the primary name
/// shows up in the popup, to keep the list tidy.
///
/// Order matters: this is the order operators see, so screen jumps
/// come first (most-used), action verbs in approximate frequency
/// order, the four economics previews + buy-suggest grouped together,
/// utility verbs last.
const KNOWN_COMMANDS: &[(&str, &str)] = &[
    ("health", "S1 Health screen"),
    ("stamps", "S2 Stamps screen"),
    ("swap", "S3 SWAP / cheques screen"),
    ("lottery", "S4 Lottery + rchash"),
    ("peers", "S6 Peers + bin saturation"),
    ("network", "S7 Network / NAT"),
    ("warmup", "S5 Warmup checklist"),
    ("api", "S8 RPC / API health"),
    ("tags", "S9 Tags / uploads"),
    ("pins", "S11 Pins screen"),
    ("topup-preview", "<batch> <amount-plur> — predict topup"),
    ("dilute-preview", "<batch> <new-depth> — predict dilute"),
    ("extend-preview", "<batch> <duration> — predict extend"),
    ("buy-preview", "<depth> <amount-plur> — predict fresh buy"),
    ("buy-suggest", "<size> <duration> — minimum (depth, amount)"),
    (
        "plan-batch",
        "<batch> [usage-thr] [ttl-thr] [extra-depth] — unified topup+dilute plan",
    ),
    (
        "check-version",
        "compare running Bee version with GitHub's latest release",
    ),
    (
        "config-doctor",
        "audit bee.yaml for deprecated keys (read-only, never modifies)",
    ),
    ("price", "fetch xBZZ → USD spot price"),
    (
        "basefee",
        "fetch Gnosis basefee + tip (requires [economics].gnosis_rpc_url)",
    ),
    (
        "probe-upload",
        "<batch> — single 4 KiB chunk, end-to-end probe",
    ),
    (
        "upload-file",
        "<path> <batch> — upload a single local file, return Swarm ref",
    ),
    (
        "upload-collection",
        "<dir> <batch> — recursive directory upload, return Swarm ref",
    ),
    (
        "feed-probe",
        "<owner> <topic> — latest update for a feed (read-only lookup)",
    ),
    (
        "feed-timeline",
        "<owner> <topic> [N] — walk a feed's history, open S14",
    ),
    (
        "manifest",
        "<ref> — open Mantaray tree browser at a reference",
    ),
    (
        "inspect",
        "<ref> — what is this? auto-detects manifest vs raw chunk",
    ),
    (
        "durability-check",
        "<ref> — walk chunk graph, report total / lost / errors",
    ),
    (
        "grantees-list",
        "<ref> — list ACT grantees on a reference (read-only)",
    ),
    (
        "watch-ref",
        "<ref> [interval] — run :durability-check every interval (default 60s)",
    ),
    (
        "watch-ref-stop",
        "[ref] — stop one :watch-ref daemon (or all if no arg)",
    ),
    (
        "pubsub-pss",
        "<topic> — subscribe to PSS messages on a topic, surface in S15",
    ),
    (
        "pubsub-gsoc",
        "<owner> <identifier> — subscribe to a GSOC SOC, surface in S15",
    ),
    (
        "pubsub-stop",
        "[sub-id] — stop one pubsub subscription (or all if no arg)",
    ),
    (
        "pubsub-filter",
        "<substring> — show only messages whose channel/preview contains substring",
    ),
    (
        "pubsub-filter-clear",
        "remove the active S15 substring filter",
    ),
    (
        "pubsub-replay",
        "<path> — load a pubsub history JSONL into the S15 timeline",
    ),
    ("watchlist", "S13 Watchlist — durability-check history"),
    (
        "fleet",
        "S15 Fleet — health roll-up across every [[nodes]] entry",
    ),
    (
        "hash",
        "<path> — Swarm reference of a local file/dir (offline)",
    ),
    ("cid", "<ref> [manifest|feed] — encode reference as CID"),
    ("depth-table", "Print canonical depth → capacity table"),
    (
        "gsoc-mine",
        "<overlay> <id> — mine a GSOC signer (CPU work)",
    ),
    (
        "pss-target",
        "<overlay> — first 4 hex chars (Bee's max prefix)",
    ),
    (
        "diagnose",
        "[--pprof[=N]] Export snapshot (+ optional CPU profile + trace)",
    ),
    ("pins-check", "Bulk integrity walk to a file"),
    ("loggers", "Dump live logger registry"),
    ("set-logger", "<expr> <level> — change a logger's verbosity"),
    ("context", "<name> — switch node profile"),
    (
        "nodes",
        "open the node picker (Ctrl-N) — switch between [[nodes]]",
    ),
    (
        "notifications",
        "open the notification history overlay (Ctrl+Alt+N)",
    ),
    ("quit", "Exit the cockpit"),
];

/// Pull the `--pprof[=N]` flag value out of a `:diagnose ...`
/// command line. Returns `Some(seconds)` when the flag is present
/// (defaulting to 60 when no `=N` is supplied), `None` when the
/// operator just typed `:diagnose`. Pure for testability.
fn parse_pprof_arg(line: &str) -> Option<u32> {
    for tok in line.split_whitespace() {
        if tok == "--pprof" {
            return Some(60);
        }
        if let Some(rest) = tok.strip_prefix("--pprof=") {
            if let Ok(n) = rest.parse::<u32>() {
                return Some(n.clamp(1, 600));
            }
        }
    }
    None
}

/// Produce the filtered list of (name, description) pairs that match
/// the buffer's first whitespace token (case-insensitive prefix). An
/// empty buffer matches everything. Pure for testability.
fn filter_command_suggestions<'a>(
    buffer: &str,
    catalog: &'a [(&'a str, &'a str)],
) -> Vec<&'a (&'a str, &'a str)> {
    let head = buffer
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_lowercase();
    catalog
        .iter()
        .filter(|(name, _)| name.starts_with(&head))
        .collect()
}

/// Resolve the line to execute when Enter is pressed in the command
/// bar. If a suggestion is highlighted in the filtered picker, the
/// line is that suggestion's name plus any args typed after the
/// first token — so arrowing the picker and pressing Enter runs the
/// *selected* command, not the half-typed prefix in the buffer.
/// When nothing matches, the raw buffer is returned unchanged so
/// `execute_command` can report it as an unknown command. Pure for
/// testability.
fn resolve_command_line(buffer: &str, suggestion_index: usize) -> String {
    let matches = filter_command_suggestions(buffer, KNOWN_COMMANDS);
    match matches.get(suggestion_index) {
        Some((name, _)) => {
            let rest = buffer
                .split_once(char::is_whitespace)
                .map(|(_, tail)| tail)
                .unwrap_or("");
            if rest.is_empty() {
                (*name).to_string()
            } else {
                format!("{name} {rest}")
            }
        }
        None => buffer.to_string(),
    }
}

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Home,
}

/// Configuration knobs the binary passes into [`App::with_overrides`].
/// Bundled in a struct so future flags don't churn the call site.
#[derive(Debug, Default)]
pub struct AppOverrides {
    /// Force ASCII glyphs.
    pub ascii: bool,
    /// Force the mono palette.
    pub no_color: bool,
    /// `--bee-bin` CLI override.
    pub bee_bin: Option<PathBuf>,
    /// `--bee-config` CLI override.
    pub bee_config: Option<PathBuf>,
    /// `--bee-log` CLI override — tail this external Bee log file.
    /// Applies to the active node at startup; overrides that node's
    /// `[[nodes]].log_file`. Ignored when `bee_bin` is set.
    pub bee_log: Option<PathBuf>,
    /// `--bee-log-cmd` CLI override — tail the stdout of this shell
    /// command for the active node at startup. Overrides that node's
    /// `[[nodes]].log_command`, and takes precedence over `bee_log`.
    /// Ignored when `bee_bin` is set.
    pub bee_log_cmd: Option<String>,
}

/// Default timeout for waiting on `/health` after spawning Bee.
/// Bee's first start can include chain-state catch-up; a generous
/// budget here saves the operator from one false "didn't come up"
/// alarm. Override later via config if needed.
const BEE_API_READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Pick the external bee-log source for a node. CLI overrides
/// (startup-only) win over the node's `[[nodes]]` config; within
/// each tier a command beats a file. `None` means no source — the
/// Bee-side log tabs stay empty for this node.
fn resolve_bee_log_source(
    cli_cmd: Option<&str>,
    cli_file: Option<&Path>,
    node_cmd: Option<&str>,
    node_file: Option<&Path>,
) -> Option<BeeLogSource> {
    if let Some(c) = cli_cmd {
        return Some(BeeLogSource::Command(c.to_string()));
    }
    if let Some(f) = cli_file {
        return Some(BeeLogSource::File(f.to_path_buf()));
    }
    if let Some(c) = node_cmd {
        return Some(BeeLogSource::Command(c.to_string()));
    }
    if let Some(f) = node_file {
        return Some(BeeLogSource::File(f.to_path_buf()));
    }
    None
}

/// Spawn the right tailer for a resolved [`BeeLogSource`] under a
/// child of `root_cancel`. Returns the receiver the App drains every
/// Tick plus the cancel token `switch_context` uses to stop this
/// tailer before re-pointing at the next node.
fn spawn_bee_log_tailer(
    source: BeeLogSource,
    root_cancel: &CancellationToken,
) -> (
    mpsc::UnboundedReceiver<(LogTab, BeeLogLine)>,
    CancellationToken,
) {
    let (tx, rx) = mpsc::unbounded_channel();
    let cancel = root_cancel.child_token();
    match source {
        BeeLogSource::File(path) => {
            // External file: tail from EOF — it pre-exists and may
            // be huge, so replaying from byte 0 would flood the pane.
            crate::bee_log_tailer::spawn(path, tx, cancel.clone(), true);
        }
        BeeLogSource::Command(cmd) => {
            crate::bee_log_tailer::spawn_command(cmd, tx, cancel.clone());
        }
    }
    (rx, cancel)
}

impl App {
    pub async fn new(tick_rate: f64, frame_rate: f64) -> color_eyre::Result<Self> {
        Self::with_overrides(tick_rate, frame_rate, AppOverrides::default()).await
    }

    /// Build an App with explicit `--ascii` / `--no-color` /
    /// `--bee-bin` / `--bee-config` overrides. Async because, when
    /// the bee paths are set, we spawn Bee and wait for its `/health`
    /// before opening the TUI.
    pub async fn with_overrides(
        tick_rate: f64,
        frame_rate: f64,
        overrides: AppOverrides,
    ) -> color_eyre::Result<Self> {
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let (cmd_status_tx, cmd_status_rx) = mpsc::unbounded_channel();
        let (durability_tx, durability_rx) = mpsc::unbounded_channel();
        let (feed_timeline_tx, feed_timeline_rx) = mpsc::unbounded_channel();
        let (pubsub_msg_tx, pubsub_msg_rx) = mpsc::unbounded_channel();
        let config = Config::new()?;

        // Optional pubsub history-file writer. Failures here aren't
        // fatal — the live tail keeps working without persistence —
        // so we log a warning and keep going.
        let pubsub_history = match config.pubsub.history_file.as_deref() {
            Some(path) => {
                let rotate_bytes = config.pubsub.rotate_size_mb.saturating_mul(1024 * 1024);
                let keep = config.pubsub.keep_files;
                match crate::pubsub::open_history_writer(path, rotate_bytes, keep).await {
                    Ok(w) => w,
                    Err(e) => {
                        tracing::warn!(target: "bee_tui::pubsub", "history file disabled: {e}");
                        None
                    }
                }
            }
            None => None,
        };
        // Install the theme first so any tracing emitted during the
        // rest of `new` already reflects the operator's choice.
        let force_no_color = overrides.no_color || theme::no_color_env();
        theme::install_with_overrides(&config.ui, force_no_color, overrides.ascii);

        // Pick the active node profile (and its URL) before spawning
        // Bee — the supervisor's /health probe needs the URL.
        let node = config
            .active_node()
            .ok_or_else(|| eyre!("no Bee node configured (config.nodes is empty)"))?;
        let api = Arc::new(ApiClient::from_node(node)?);
        // The active node's declarative log source (file path +
        // command), captured before the `node` borrow ends. Feeds the
        // bee-log tailer when bee-tui connects to an external
        // (un-supervised) Bee.
        let active_node_log_file = node.log_file.clone();
        let active_node_log_command = node.log_command.clone();

        // Resolve the bee paths: CLI flags > [bee] config block > unset.
        let bee_bin = overrides
            .bee_bin
            .or_else(|| config.bee.as_ref().map(|b| b.bin.clone()));
        let bee_config = overrides
            .bee_config
            .or_else(|| config.bee.as_ref().map(|b| b.config.clone()));
        // [bee.logs] sub-config; defaults if [bee] is set but
        // [bee.logs] isn't.
        let bee_logs = config
            .bee
            .as_ref()
            .map(|b| b.logs.clone())
            .unwrap_or_default();
        let bee_supervisor_policy = config
            .bee
            .as_ref()
            .map(|b| b.supervisor.clone())
            .unwrap_or_default();
        let (supervisor, supervisor_watchdog) = match (bee_bin, bee_config) {
            (Some(bin), Some(cfg)) => {
                eprintln!("bee-tui: spawning bee {bin:?} --config {cfg:?}");
                let mut sup = BeeSupervisor::spawn(&bin, &cfg, bee_logs.clone())?;
                eprintln!(
                    "bee-tui: log → {} (will appear in the cockpit's bottom pane)",
                    sup.log_path().display()
                );
                eprintln!(
                    "bee-tui: waiting for {} to respond on /health (up to {:?})...",
                    api.url, BEE_API_READY_TIMEOUT
                );
                sup.wait_for_api(&api.url, BEE_API_READY_TIMEOUT).await?;
                eprintln!("bee-tui: bee ready, opening cockpit");
                let watchdog = if bee_supervisor_policy.auto_restart {
                    eprintln!(
                        "bee-tui: auto-restart on — max {} per hour, backoff {}-{}s",
                        bee_supervisor_policy.max_restarts_per_hour,
                        bee_supervisor_policy.backoff_initial_secs,
                        bee_supervisor_policy.backoff_max_secs,
                    );
                    Some(SupervisorWatchdog {
                        bin: bin.clone(),
                        config: cfg.clone(),
                        logs: bee_logs.clone(),
                        policy: bee_supervisor_policy.clone(),
                        restart_history: std::collections::VecDeque::new(),
                        next_attempt_at: None,
                        restart_count: 0,
                    })
                } else {
                    None
                };
                (Some(sup), watchdog)
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(eyre!(
                    "[bee].bin and [bee].config must both be set (or both unset). \
                     Use --bee-bin AND --bee-config, or both fields in config.toml."
                ));
            }
            (None, None) => (None, None),
        };

        // Spawn the watch / informer hub. Pollers attach to children
        // of `root_cancel`, so quitting cancels everything in one go.
        // The cadence preset comes from `[ui].refresh` — operators
        // who want the original 2 s health stream can opt into
        // `"live"`; the default is "calmer" (4 s health, 10 s
        // topology).
        let refresh = RefreshProfile::from_config(&config.ui.refresh);
        let root_cancel = CancellationToken::new();
        let watch = BeeWatch::start_with_profile(api.clone(), &root_cancel, refresh);
        let health_rx = watch.health();

        // Cost-context poller — opt-in via `[economics].enable_market_tile`.
        // When off, no outbound traffic and S3 SWAP renders identically
        // to v1.3 (no Market tile slot).
        let market_rx = if config.economics.enable_market_tile {
            Some(economics_oracle::spawn_poller(
                config.economics.gnosis_rpc_url.clone(),
                root_cancel.child_token(),
            ))
        } else {
            None
        };

        // Fleet poller — fans a cheap /health + /status + /stamps
        // probe out to **every** configured node every 10 s. Lives
        // across context switches because the S15 screen wants to
        // surface other nodes' state regardless of which one the
        // operator's currently driving. Resync mpsc is the "r" key
        // path for impatient re-probes.
        let (fleet_rx, fleet_resync_tx) = crate::fleet::spawn_poller(
            config.nodes.clone(),
            root_cancel.child_token(),
            std::time::Duration::from_secs(10),
        );

        let screens = build_screens(
            &api,
            &watch,
            market_rx,
            fleet_rx.clone(),
            fleet_resync_tx.clone(),
        );
        // Bottom log pane subscribes to the bee::http capture set up
        // by logging::init for its `bee::http` tab. The four severity
        // tabs + "Bee HTTP" tab populate from the supervisor's log
        // tail (increment 3+); for now they show placeholders.
        let (persisted, state_path) = State::load();
        let initial_tab = LogTab::from_kebab(&persisted.log_pane_active_tab);
        let mut log_pane = LogPane::new(
            log_capture::handle(),
            initial_tab,
            persisted.log_pane_height,
        );
        if let Some(c) = log_capture::cockpit_handle() {
            log_pane.set_cockpit_capture(c);
        }

        // Resolve where the Bee-side log tabs get their content.
        // Skipped in supervisor mode (the supervised child's captured
        // log is tailed instead). Otherwise: explicit config wins
        // (`--bee-log-cmd` / `--bee-log` / `[[nodes]].log_command` /
        // `log_file`); failing that, auto-discovery inspects the
        // local Bee process via `/proc` to find where its stdout
        // goes. A `log_source_hint` carries the operator-facing
        // explanation when a local Bee was found but its log can't
        // be captured (e.g. it logs to a bare terminal).
        let (external_log_source, log_source_hint) = if supervisor.is_some() {
            (None, None)
        } else {
            match resolve_bee_log_source(
                overrides.bee_log_cmd.as_deref(),
                overrides.bee_log.as_deref(),
                active_node_log_command.as_deref(),
                active_node_log_file.as_deref(),
            ) {
                Some(src) => (Some(src), None),
                None => match bee_log_discover::discover(&api.url) {
                    DiscoveryResult::Found(src) => (Some(src), None),
                    DiscoveryResult::Unsupported(msg) => (None, Some(msg)),
                    DiscoveryResult::NotApplicable => (None, None),
                },
            }
        };
        // The Bee-side log tabs have a real source whenever either a
        // supervisor or an external tailer is wired up — drives the
        // placeholder text ("awaiting…" vs. "no bee log source").
        log_pane.set_spawn_active(supervisor.is_some() || external_log_source.is_some());
        // Surface the outcome on the Cockpit log tab (and thus in
        // `:diagnose` bundles) so it's traceable without watching
        // the Bee-side placeholder.
        match &external_log_source {
            Some(BeeLogSource::File(p)) => {
                tracing::info!("bee log: tailing file {}", p.display())
            }
            Some(BeeLogSource::Command(c)) => {
                tracing::info!("bee log: tailing command `{c}`")
            }
            None => {}
        }
        if let Some(hint) = &log_source_hint {
            tracing::warn!("bee log auto-discovery: {hint}");
        }
        log_pane.set_log_source_hint(log_source_hint);

        // Spawn the bee-log tailer. Two mutually-exclusive cases:
        //   - supervisor mode: tail the captured child log from
        //     byte 0 (fresh file — we want Bee's startup logs).
        //   - external mode: tail a configured file (from EOF) or a
        //     command's stdout — see `spawn_bee_log_tailer`.
        // The tailer forwards `(LogTab, BeeLogLine)` pairs down an
        // mpsc the App drains every Tick. `bee_log_tailer_cancel` is
        // retained only for the external case so `switch_context`
        // can stop + re-spawn it for the new node; the supervisor's
        // tailer lives for the whole session under `root_cancel`.
        let (bee_log_rx, bee_log_tailer_cancel) = if let Some(sup) = supervisor.as_ref() {
            let (tx, rx) = mpsc::unbounded_channel();
            crate::bee_log_tailer::spawn(
                sup.log_path().to_path_buf(),
                tx,
                root_cancel.child_token(),
                false,
            );
            (Some(rx), None)
        } else if let Some(source) = external_log_source {
            let (rx, cancel) = spawn_bee_log_tailer(source, &root_cancel);
            (Some(rx), Some(cancel))
        } else {
            (None, None)
        };

        // Optional Prometheus `/metrics` endpoint. Off by default;
        // when `[metrics].enabled = true` we spawn the server under
        // `root_cancel` so it dies with the cockpit. Failures here
        // are non-fatal — surface a tracing error and keep going,
        // since a port-conflict shouldn't block the operator from
        // using the cockpit itself.
        if config.metrics.enabled {
            match config.metrics.addr.parse::<std::net::SocketAddr>() {
                Ok(bind_addr) => {
                    let render_fn = build_metrics_render_fn(watch.clone(), log_capture::handle());
                    let cancel = root_cancel.child_token();
                    match crate::metrics_server::spawn(bind_addr, render_fn, cancel).await {
                        Ok(actual) => {
                            eprintln!(
                                "bee-tui: metrics endpoint serving /metrics on http://{actual}"
                            );
                        }
                        Err(e) => {
                            tracing::error!(
                                "metrics: failed to start endpoint on {bind_addr}: {e}"
                            );
                        }
                    }
                }
                Err(e) => {
                    tracing::error!(
                        "metrics: invalid [metrics].addr {:?}: {e}",
                        config.metrics.addr
                    );
                }
            }
        }

        let config_alerts_debounce = config.alerts.debounce_secs;

        Ok(Self {
            tick_rate,
            frame_rate,
            screens,
            current_screen: 0,
            log_pane,
            state_path,
            should_quit: false,
            should_suspend: false,
            config,
            mode: Mode::Home,
            last_tick_key_events: Vec::new(),
            action_tx,
            action_rx,
            root_cancel,
            api,
            watch,
            health_rx,
            command_buffer: None,
            command_suggestion_index: 0,
            command_status: None,
            help_visible: false,
            quit_pending: None,
            supervisor,
            bee_status: BeeStatus::Running,
            bee_log_rx,
            bee_log_tailer_cancel,
            cmd_status_tx,
            cmd_status_rx,
            durability_tx,
            durability_rx,
            feed_timeline_tx,
            feed_timeline_rx,
            watch_refs: std::collections::HashMap::new(),
            pubsub_subs: std::collections::HashMap::new(),
            pubsub_history,
            pubsub_msg_tx,
            pubsub_msg_rx,
            alert_state: crate::alerts::AlertState::new(config_alerts_debounce),
            nodes_picker_visible: false,
            nodes_picker_selected: 0,
            help_page: HelpPage::Keys,
            fleet_rx,
            fleet_resync_tx,
            batch_modal: BatchModal::default(),
            supervisor_watchdog,
            fleet_aggregator: FleetAggregator::default(),
            log_fullscreen: false,
            notifications: crate::notifications::NotificationCenter::default(),
            notifications_overlay_visible: false,
        })
    }

    pub async fn run(&mut self) -> color_eyre::Result<()> {
        let mut tui = Tui::new()?
            // .mouse(true) // uncomment this line to enable mouse support
            .tick_rate(self.tick_rate)
            .frame_rate(self.frame_rate);
        tui.enter()?;

        let tx = self.action_tx.clone();
        let cfg = self.config.clone();
        let size = tui.size()?;
        for component in self.iter_components_mut() {
            component.register_action_handler(tx.clone())?;
            component.register_config_handler(cfg.clone())?;
            component.init(size)?;
        }

        let action_tx = self.action_tx.clone();
        loop {
            self.handle_events(&mut tui).await?;
            self.handle_actions(&mut tui)?;
            if self.should_suspend {
                tui.suspend()?;
                action_tx.send(Action::Resume)?;
                action_tx.send(Action::ClearScreen)?;
                // tui.mouse(true);
                tui.enter()?;
            } else if self.should_quit {
                tui.stop()?;
                break;
            }
        }
        // Unwind every spawned task before tearing down the terminal.
        self.watch.shutdown();
        self.root_cancel.cancel();
        // Persist UI state (last tab + height) so the next launch
        // restores the operator's preference. Best-effort — failures
        // log a warning but never block quit.
        let snapshot = State {
            log_pane_height: self.log_pane.height(),
            log_pane_active_tab: self.log_pane.active_tab().to_kebab().to_string(),
        };
        snapshot.save(&self.state_path);
        // SIGTERM Bee (pgroup) and wait for clean exit. Done before
        // tui.exit() so any "bee shutting down" messages still land
        // in the supervisor's log file (no race with terminal teardown).
        if let Some(sup) = self.supervisor.take() {
            let final_status = sup.shutdown_default().await;
            tracing::info!("bee child exited: {}", final_status.label());
        }
        tui.exit()?;
        Ok(())
    }

    async fn handle_events(&mut self, tui: &mut Tui) -> color_eyre::Result<()> {
        let Some(event) = tui.next_event().await else {
            return Ok(());
        };
        let action_tx = self.action_tx.clone();
        // Sample modal state both before and after handling: a key
        // that *opens* a modal (`?` → help) only flips state inside
        // handle, but the same key shouldn't propagate to screens;
        // a key that *closes* one (Esc on help) flips it the other
        // way but also shouldn't propagate. Either side of the
        // transition counts as "modal" for swallowing purposes.
        let modal_before = self.command_buffer.is_some()
            || self.help_visible
            || self.nodes_picker_visible
            || self.notifications_overlay_visible
            || self.batch_modal.visible
            || self.log_pane.filter_prompt_visible();
        match event {
            Event::Quit => action_tx.send(Action::Quit)?,
            Event::Tick => action_tx.send(Action::Tick)?,
            Event::Render => action_tx.send(Action::Render)?,
            Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
            Event::Key(key) => self.handle_key_event(key)?,
            _ => {}
        }
        let modal_after = self.command_buffer.is_some()
            || self.help_visible
            || self.nodes_picker_visible
            || self.notifications_overlay_visible
            || self.batch_modal.visible
            || self.log_pane.filter_prompt_visible();
        // Non-key events (Tick / Resize / Render) always propagate
        // so screens keep refreshing under modals.
        let propagate = !((modal_before || modal_after) && matches!(event, Event::Key(_)));
        if propagate {
            match event {
                // Key events reach ONLY the active screen. Delivering
                // them to every screen let a *background* screen act
                // on a keystroke meant for the foreground one — most
                // visibly, S15 Fleet's `Enter` → switch-context
                // binding fired on *every* Enter (drilling a peer on
                // S6, expanding a manifest fork on S11, …), which
                // rebuilt all screens and discarded the action the
                // operator actually wanted. Per-screen keymaps are
                // inherently about the screen the operator is looking
                // at, so this is also just correct.
                Event::Key(_) => {
                    if let Some(screen) = self.screens.get_mut(self.current_screen) {
                        if let Some(action) = screen.handle_events(Some(event))? {
                            action_tx.send(action)?;
                        }
                    }
                }
                // Tick / Render / Resize reach every component so
                // background screens keep their data fresh and the
                // log pane keeps ticking.
                _ => {
                    for component in self.iter_components_mut() {
                        if let Some(action) = component.handle_events(Some(event.clone()))? {
                            action_tx.send(action)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Iterate every component (screens + log pane) for uniform
    /// lifecycle ticks. Returns trait objects so the heterogeneous
    /// `LogPane` (a concrete type for direct method access in the
    /// app layer) walks alongside the boxed screens.
    fn iter_components_mut(&mut self) -> impl Iterator<Item = &mut dyn Component> {
        self.screens
            .iter_mut()
            .map(|c| c.as_mut() as &mut dyn Component)
            .chain(std::iter::once(&mut self.log_pane as &mut dyn Component))
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        // While a `:command` is being typed every key edits the
        // buffer or commits / cancels the line. No other keymap
        // applies.
        if self.command_buffer.is_some() {
            self.handle_command_mode_key(key)?;
            return Ok(());
        }
        // While the `?` help overlay is up, only Esc / ? / q close
        // it. Don't propagate to components or process other keys
        // — the operator is reading reference, not driving.
        if self.help_visible {
            match key.code {
                crossterm::event::KeyCode::Esc
                | crossterm::event::KeyCode::Char('?')
                | crossterm::event::KeyCode::Char('q') => {
                    self.help_visible = false;
                }
                crossterm::event::KeyCode::Tab | crossterm::event::KeyCode::BackTab => {
                    self.help_page = match self.help_page {
                        HelpPage::Keys => HelpPage::Verbs,
                        HelpPage::Verbs => HelpPage::Keys,
                    };
                }
                _ => {}
            }
            return Ok(());
        }
        // Node-picker overlay key routing. Only ↑/↓, Enter, Esc,
        // and Ctrl-N (to dismiss) reach this branch; everything
        // else is swallowed so a stray keystroke can't switch
        // screens behind the overlay.
        if self.nodes_picker_visible {
            let len = self.config.nodes.len();
            match key.code {
                crossterm::event::KeyCode::Esc => {
                    self.nodes_picker_visible = false;
                }
                crossterm::event::KeyCode::Char('n')
                    if key.modifiers == crossterm::event::KeyModifiers::CONTROL =>
                {
                    self.nodes_picker_visible = false;
                }
                crossterm::event::KeyCode::Up | crossterm::event::KeyCode::Char('k') if len > 0 => {
                    self.nodes_picker_selected = (self.nodes_picker_selected + len - 1) % len;
                }
                crossterm::event::KeyCode::Down | crossterm::event::KeyCode::Char('j')
                    if len > 0 =>
                {
                    self.nodes_picker_selected = (self.nodes_picker_selected + 1) % len;
                }
                crossterm::event::KeyCode::Enter if len > 0 => {
                    let target = self.config.nodes[self.nodes_picker_selected].name.clone();
                    self.nodes_picker_visible = false;
                    // No-op when the cursor was already on the
                    // active node; avoids a needless watch-hub
                    // rebuild.
                    if target != self.api.name {
                        self.command_status = Some(match self.switch_context(&target) {
                            Ok(()) => CommandStatus::Info(format!(
                                "switched to context {target} ({})",
                                self.api.url
                            )),
                            Err(e) => CommandStatus::Err(format!("context switch failed: {e}")),
                        });
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        // Log-pane filter prompt routing. While `/` has opened
        // the prompt, every keystroke flows into the input buffer
        // (or commits / cancels). Same modal discipline as help /
        // picker / batch modal.
        if self.log_pane.filter_prompt_visible() {
            use crossterm::event::KeyCode;
            match key.code {
                KeyCode::Esc => self.log_pane.cancel_filter_prompt(),
                KeyCode::Enter => self.log_pane.commit_filter_prompt(),
                KeyCode::Char(c) => self.log_pane.push_filter_char(c),
                KeyCode::Backspace => self.log_pane.pop_filter_char(),
                _ => {}
            }
            return Ok(());
        }
        // `/` opens the log-pane filter prompt — same key as
        // grep-in-pane in less / k9s / lazygit. Operators don't
        // have to know "this is for the log pane"; it's the only
        // pane with a filter so context is implicit.
        if matches!(key.code, crossterm::event::KeyCode::Char('/'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.log_pane.open_filter_prompt();
            return Ok(());
        }
        // Esc with no modal open clears any active log filter so
        // operators can drop back to the unfiltered tail without
        // flipping tabs or retyping.
        if matches!(key.code, crossterm::event::KeyCode::Esc)
            && self.log_pane.active_filter().is_some()
        {
            self.log_pane.clear_filter();
            return Ok(());
        }
        // Notifications history overlay routing (must precede the
        // Ctrl-N picker so Ctrl-Alt-N doesn't fall through to it).
        if self.notifications_overlay_visible {
            if matches!(key.code, crossterm::event::KeyCode::Esc) {
                self.notifications_overlay_visible = false;
            }
            return Ok(());
        }
        // `Ctrl+Alt+N` opens the notification-history overlay.
        // Mirrors the `:notifications` verb. Same modal pattern as
        // help / picker: keys other than Esc are swallowed while
        // it's open.
        if matches!(key.code, crossterm::event::KeyCode::Char('n'))
            && key.modifiers
                == (crossterm::event::KeyModifiers::CONTROL | crossterm::event::KeyModifiers::ALT)
        {
            self.notifications_overlay_visible = true;
            // Opening the overlay = the operator has now seen
            // everything; clear the top-bar unread chip.
            self.notifications.mark_all_read();
            return Ok(());
        }
        // Ctrl-N opens the node-picker overlay (mirrors `:nodes`).
        // Captured at the app level so every screen gets it without
        // each one wiring it. Plain `n` stays free for in-screen use.
        if matches!(key.code, crossterm::event::KeyCode::Char('n'))
            && key.modifiers == crossterm::event::KeyModifiers::CONTROL
        {
            let active = self
                .config
                .nodes
                .iter()
                .position(|n| n.name == self.api.name)
                .unwrap_or(0);
            self.nodes_picker_selected = active;
            self.nodes_picker_visible = true;
            return Ok(());
        }
        // Batch-economics modal key routing. Once visible, the modal
        // swallows every keystroke until Esc dismisses it — same
        // discipline as help / picker.
        if self.batch_modal.visible {
            self.handle_batch_modal_key(key);
            return Ok(());
        }
        // `E` opens the batch-economics modal. Captured at app level
        // (not S3-only) because operators often start the preview
        // flow from S2 Stamps too. Plain `e` stays free for any
        // future in-screen use (today nothing binds plain `e`).
        if matches!(key.code, crossterm::event::KeyCode::Char('E'))
            && key.modifiers == crossterm::event::KeyModifiers::SHIFT
        {
            self.batch_modal = BatchModal {
                visible: true,
                ..Default::default()
            };
            return Ok(());
        }
        // `Shift+L` toggles fullscreen log mode — collapses the
        // active screen body and lets the log pane fill the
        // middle of the cockpit. Same data, same tabs, same
        // filter (if any) — just bigger. Press again to return.
        if matches!(key.code, crossterm::event::KeyCode::Char('L'))
            && key.modifiers == crossterm::event::KeyModifiers::SHIFT
        {
            self.log_fullscreen = !self.log_fullscreen;
            return Ok(());
        }
        // `?` opens the help overlay. We capture it at the app level
        // so every screen gets the overlay for free without each one
        // having to wire its own.
        if matches!(key.code, crossterm::event::KeyCode::Char('?')) {
            self.help_visible = true;
            return Ok(());
        }
        let action_tx = self.action_tx.clone();
        // ':' opens the command bar.
        if matches!(key.code, crossterm::event::KeyCode::Char(':')) {
            self.command_buffer = Some(String::new());
            self.command_status = None;
            return Ok(());
        }
        // Tab / Shift+Tab keep working as a quick screen-cycle
        // shortcut even after the `:command` bar lands. crossterm
        // surfaces Shift+Tab as `BackTab` (a separate KeyCode rather
        // than Tab + the Shift modifier), so both branches are needed.
        if matches!(key.code, crossterm::event::KeyCode::Tab) {
            if !self.screens.is_empty() {
                self.current_screen = (self.current_screen + 1) % self.screens.len();
                debug!(
                    "switched to screen {}",
                    SCREEN_NAMES.get(self.current_screen).unwrap_or(&"?")
                );
            }
            return Ok(());
        }
        if matches!(key.code, crossterm::event::KeyCode::BackTab) {
            if !self.screens.is_empty() {
                let len = self.screens.len();
                self.current_screen = (self.current_screen + len - 1) % len;
                debug!(
                    "switched to screen {}",
                    SCREEN_NAMES.get(self.current_screen).unwrap_or(&"?")
                );
            }
            return Ok(());
        }
        // Direct numeric jumps for the cockpit's 15 screens. Plain
        // digits 1-9 jump to S1-S9 (Health through Tags); 0 jumps
        // to S10 (Pins). Alt+1..Alt+5 reach the second-row screens
        // S11-S15 (Manifest, Watchlist, FeedTimeline, Pubsub, Fleet)
        // — Alt keeps the plain digit row available for any future
        // in-screen numeric input without conflict.
        if let crossterm::event::KeyCode::Char(c) = key.code {
            if key.modifiers == crossterm::event::KeyModifiers::NONE {
                let idx = match c {
                    '1'..='9' => Some((c as usize) - ('1' as usize)),
                    '0' => Some(9),
                    _ => None,
                };
                if let Some(i) = idx {
                    if i < self.screens.len() {
                        self.current_screen = i;
                        debug!(
                            "switched to screen {}",
                            SCREEN_NAMES.get(self.current_screen).unwrap_or(&"?")
                        );
                        return Ok(());
                    }
                }
            }
            if key.modifiers == crossterm::event::KeyModifiers::ALT
                && let Some(d) = c.to_digit(10)
                && (1..=9).contains(&d)
            {
                let i = 10 + (d as usize) - 1;
                if i < self.screens.len() {
                    self.current_screen = i;
                    debug!(
                        "switched to screen {}",
                        SCREEN_NAMES.get(self.current_screen).unwrap_or(&"?")
                    );
                    return Ok(());
                }
            }
        }
        // Log-pane controls. `[` / `]` cycle tabs (lazygit / k9s
        // pattern, no conflict with screen-cycling Tab/Shift+Tab).
        // `+` / `-` resize the pane in 1-line steps, clamped to
        // [LOG_PANE_MIN_HEIGHT, LOG_PANE_MAX_HEIGHT]. The state is
        // persisted on quit.
        if matches!(key.code, crossterm::event::KeyCode::Char('['))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.log_pane.prev_tab();
            return Ok(());
        }
        if matches!(key.code, crossterm::event::KeyCode::Char(']'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.log_pane.next_tab();
            return Ok(());
        }
        if matches!(key.code, crossterm::event::KeyCode::Char('+'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.log_pane.grow();
            return Ok(());
        }
        if matches!(key.code, crossterm::event::KeyCode::Char('-'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            self.log_pane.shrink();
            return Ok(());
        }
        // Log-pane scroll. Shift+Up/Down step one line; Shift+PgUp/PgDn
        // step ten; Shift+End resumes tail. The Shift modifier
        // distinguishes from in-screen scroll (j/k/PgUp/PgDn) bound
        // by S2/S6/S9 — those keep working without conflict.
        if key.modifiers == crossterm::event::KeyModifiers::SHIFT {
            match key.code {
                crossterm::event::KeyCode::Up => {
                    self.log_pane.scroll_up(1);
                    return Ok(());
                }
                crossterm::event::KeyCode::Down => {
                    self.log_pane.scroll_down(1);
                    return Ok(());
                }
                crossterm::event::KeyCode::PageUp => {
                    self.log_pane.scroll_up(10);
                    return Ok(());
                }
                crossterm::event::KeyCode::PageDown => {
                    self.log_pane.scroll_down(10);
                    return Ok(());
                }
                crossterm::event::KeyCode::End => {
                    self.log_pane.resume_tail();
                    return Ok(());
                }
                // Horizontal pan for long Bee log lines. 8 chars per
                // keystroke feels live without making the operator
                // hold the key; `Shift+End` resets both axes via
                // resume_tail() so there's no separate "back to
                // left edge" binding.
                crossterm::event::KeyCode::Left => {
                    self.log_pane.scroll_left(8);
                    return Ok(());
                }
                crossterm::event::KeyCode::Right => {
                    self.log_pane.scroll_right(8);
                    return Ok(());
                }
                _ => {}
            }
        }
        // `q` is the easy-to-misclick exit. Require a double-tap
        // within `QUIT_CONFIRM_WINDOW` so a stray keystroke doesn't
        // kill an active monitoring session. `Ctrl+C` / `Ctrl+D`
        // remain wired through the keybindings system as immediate
        // quit — escape hatches if the cockpit ever stops responding.
        if matches!(key.code, crossterm::event::KeyCode::Char('q'))
            && key.modifiers == crossterm::event::KeyModifiers::NONE
        {
            match resolve_quit_press(self.quit_pending, Instant::now(), QUIT_CONFIRM_WINDOW) {
                QuitResolution::Confirm => {
                    self.quit_pending = None;
                    self.action_tx.send(Action::Quit)?;
                }
                QuitResolution::Pending => {
                    self.quit_pending = Some(Instant::now());
                    self.command_status = Some(CommandStatus::Info(
                        "press q again to quit (Esc cancels)".into(),
                    ));
                }
            }
            return Ok(());
        }
        // Any other key resets the pending-quit window so the operator
        // doesn't accidentally confirm later from a forgotten first
        // tap.
        if self.quit_pending.is_some() {
            self.quit_pending = None;
        }
        let Some(keymap) = self.config.keybindings.0.get(&self.mode) else {
            return Ok(());
        };
        match keymap.get(&vec![key]) {
            Some(action) => {
                info!("Got action: {action:?}");
                action_tx.send(action.clone())?;
            }
            _ => {
                // If the key was not handled as a single key action,
                // then consider it for multi-key combinations.
                self.last_tick_key_events.push(key);

                // Check for multi-key combinations
                if let Some(action) = keymap.get(&self.last_tick_key_events) {
                    info!("Got action: {action:?}");
                    action_tx.send(action.clone())?;
                }
            }
        }
        Ok(())
    }

    fn handle_command_mode_key(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        use crossterm::event::KeyCode;
        let buf = match self.command_buffer.as_mut() {
            Some(b) => b,
            None => return Ok(()),
        };
        match key.code {
            KeyCode::Esc => {
                // Cancel without dispatching.
                self.command_buffer = None;
                self.command_suggestion_index = 0;
            }
            KeyCode::Enter => {
                // Run the picker's highlighted suggestion, not the
                // raw buffer — see `resolve_command_line`.
                let line = resolve_command_line(buf, self.command_suggestion_index);
                self.command_buffer = None;
                self.command_suggestion_index = 0;
                self.execute_command(&line)?;
            }
            KeyCode::Up => {
                // Walk up the filtered suggestion list. Saturates at
                // 0 so a stray Up doesn't wrap unexpectedly.
                self.command_suggestion_index = self.command_suggestion_index.saturating_sub(1);
            }
            KeyCode::Down => {
                let n = filter_command_suggestions(buf, KNOWN_COMMANDS).len();
                if n > 0 && self.command_suggestion_index + 1 < n {
                    self.command_suggestion_index += 1;
                }
            }
            KeyCode::Tab => {
                // Autocomplete: replace the buffer's first token with
                // the highlighted suggestion's name and append a
                // space so the operator can type args immediately.
                let matches = filter_command_suggestions(buf, KNOWN_COMMANDS);
                if let Some((name, _)) = matches.get(self.command_suggestion_index) {
                    let rest = buf
                        .split_once(char::is_whitespace)
                        .map(|(_, tail)| tail)
                        .unwrap_or("");
                    let new = if rest.is_empty() {
                        format!("{name} ")
                    } else {
                        format!("{name} {rest}")
                    };
                    buf.clear();
                    buf.push_str(&new);
                    self.command_suggestion_index = 0;
                }
            }
            KeyCode::Backspace => {
                buf.pop();
                self.command_suggestion_index = 0;
            }
            KeyCode::Char(c) => {
                buf.push(c);
                self.command_suggestion_index = 0;
            }
            _ => {}
        }
        Ok(())
    }

    /// Resolve a `:command` token to the action it represents.
    /// Empty input is a silent no-op (operator typed `:` then Enter).
    fn execute_command(&mut self, line: &str) -> color_eyre::Result<()> {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            return Ok(());
        }
        let head = trimmed.split_whitespace().next().unwrap_or("");
        match head {
            "q" | "quit" => {
                self.action_tx.send(Action::Quit)?;
                self.command_status = Some(CommandStatus::Info("quitting".into()));
            }
            "diagnose" | "diag" => {
                let pprof_secs = parse_pprof_arg(trimmed);
                if let Some(secs) = pprof_secs {
                    self.command_status = Some(self.start_diagnose_with_pprof(secs));
                } else {
                    self.command_status = Some(match self.export_diagnostic_bundle() {
                        Ok(path) => CommandStatus::Info(format!(
                            "diagnostic bundle exported to {}",
                            path.display()
                        )),
                        Err(e) => CommandStatus::Err(format!("diagnose failed: {e}")),
                    });
                }
            }
            "pins-check" => {
                // `:pins-check` keeps the legacy bulk-check-to-file behaviour;
                // `:pins` (without `-check`) now jumps to the S11 screen via
                // the screen-name catch-all below. The two are deliberately
                // distinct so an operator who types `:pins` doesn't kick off
                // a many-minute integrity walk by accident.
                self.command_status = Some(match self.start_pins_check() {
                    Ok(path) => CommandStatus::Info(format!(
                        "pins integrity check running → {} (tail to watch progress)",
                        path.display()
                    )),
                    Err(e) => CommandStatus::Err(format!("pins-check failed to start: {e}")),
                });
            }
            "loggers" => {
                self.command_status = Some(match self.start_loggers_dump() {
                    Ok(path) => CommandStatus::Info(format!(
                        "loggers snapshot writing → {} (open when ready)",
                        path.display()
                    )),
                    Err(e) => CommandStatus::Err(format!("loggers failed to start: {e}")),
                });
            }
            "set-logger" => {
                let mut parts = trimmed.split_whitespace();
                let _ = parts.next(); // command head
                let expr = parts.next().unwrap_or("");
                let level = parts.next().unwrap_or("");
                if expr.is_empty() || level.is_empty() {
                    self.command_status = Some(CommandStatus::Err(
                        "usage: :set-logger <expr> <level>  (level: none|error|warning|info|debug|all; expr: e.g. node/pushsync or '.' for all)"
                            .into(),
                    ));
                    return Ok(());
                }
                self.start_set_logger(expr.to_string(), level.to_string());
                self.command_status = Some(CommandStatus::Info(format!(
                    "set-logger {expr:?} → {level:?} (PUT in-flight; check :loggers to verify)"
                )));
            }
            "topup-preview" => {
                self.command_status = Some(self.run_topup_preview(trimmed));
            }
            "dilute-preview" => {
                self.command_status = Some(self.run_dilute_preview(trimmed));
            }
            "extend-preview" => {
                self.command_status = Some(self.run_extend_preview(trimmed));
            }
            "buy-preview" => {
                self.command_status = Some(self.run_buy_preview(trimmed));
            }
            "buy-suggest" => {
                self.command_status = Some(self.run_buy_suggest(trimmed));
            }
            "plan-batch" => {
                self.command_status = Some(self.run_plan_batch(trimmed));
            }
            "check-version" => {
                self.command_status = Some(self.run_check_version());
            }
            "config-doctor" => {
                self.command_status = Some(self.run_config_doctor());
            }
            "price" => {
                self.command_status = Some(self.run_price());
            }
            "basefee" => {
                self.command_status = Some(self.run_basefee());
            }
            "probe-upload" => {
                self.command_status = Some(self.run_probe_upload(trimmed));
            }
            "upload-file" => {
                self.command_status = Some(self.run_upload_file(trimmed));
            }
            "upload-collection" => {
                self.command_status = Some(self.run_upload_collection(trimmed));
            }
            "feed-probe" => {
                self.command_status = Some(self.run_feed_probe(trimmed));
            }
            "feed-timeline" => {
                self.command_status = Some(self.run_feed_timeline(trimmed));
            }
            "hash" => {
                self.command_status = Some(self.run_hash(trimmed));
            }
            "cid" => {
                self.command_status = Some(self.run_cid(trimmed));
            }
            "depth-table" => {
                self.command_status = Some(self.run_depth_table());
            }
            "gsoc-mine" => {
                self.command_status = Some(self.run_gsoc_mine(trimmed));
            }
            "pss-target" => {
                self.command_status = Some(self.run_pss_target(trimmed));
            }
            "manifest" => {
                self.command_status = Some(self.run_manifest(trimmed));
            }
            "inspect" => {
                self.command_status = Some(self.run_inspect(trimmed));
            }
            "durability-check" => {
                self.command_status = Some(self.run_durability_check(trimmed));
            }
            "grantees-list" => {
                self.command_status = Some(self.run_grantees_list(trimmed));
            }
            "watch-ref" => {
                self.command_status = Some(self.run_watch_ref(trimmed));
            }
            "watch-ref-stop" => {
                self.command_status = Some(self.run_watch_ref_stop(trimmed));
            }
            "pubsub-pss" => {
                self.command_status = Some(self.run_pubsub_pss(trimmed));
            }
            "pubsub-gsoc" => {
                self.command_status = Some(self.run_pubsub_gsoc(trimmed));
            }
            "pubsub-stop" => {
                self.command_status = Some(self.run_pubsub_stop(trimmed));
            }
            "pubsub-filter" => {
                self.command_status = Some(self.run_pubsub_filter(trimmed));
            }
            "pubsub-filter-clear" => {
                self.command_status = Some(self.run_pubsub_filter_clear());
            }
            "pubsub-replay" => {
                self.command_status = Some(self.run_pubsub_replay(trimmed));
            }
            "nodes" => {
                // Open the picker overlay. Cursor lands on the
                // currently active node so Enter on an unchanged
                // selection is a cheap no-op (matches Esc).
                let active = self
                    .config
                    .nodes
                    .iter()
                    .position(|n| n.name == self.api.name)
                    .unwrap_or(0);
                self.nodes_picker_selected = active;
                self.nodes_picker_visible = true;
            }
            "notifications" => {
                self.notifications_overlay_visible = true;
                self.notifications.mark_all_read();
            }
            "context" | "ctx" => {
                let target = trimmed.split_whitespace().nth(1).unwrap_or("");
                if target.is_empty() {
                    let known: Vec<String> =
                        self.config.nodes.iter().map(|n| n.name.clone()).collect();
                    self.command_status = Some(CommandStatus::Err(format!(
                        "usage: :context <name>  (known: {})",
                        known.join(", ")
                    )));
                    return Ok(());
                }
                self.command_status = Some(match self.switch_context(target) {
                    Ok(()) => CommandStatus::Info(format!(
                        "switched to context {target} ({})",
                        self.api.url
                    )),
                    Err(e) => CommandStatus::Err(format!("context switch failed: {e}")),
                });
            }
            screen
                if SCREEN_NAMES
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(screen)) =>
            {
                if let Some(idx) = SCREEN_NAMES
                    .iter()
                    .position(|name| name.eq_ignore_ascii_case(screen))
                {
                    self.current_screen = idx;
                    self.command_status =
                        Some(CommandStatus::Info(format!("→ {}", SCREEN_NAMES[idx])));
                }
            }
            other => {
                self.command_status = Some(CommandStatus::Err(format!(
                    "unknown command: {other:?} (try :health, :stamps, :swap, :lottery, :peers, :network, :warmup, :api, :tags, :pins, :manifest, :inspect, :diagnose, :pins-check, :loggers, :set-logger, :topup-preview, :dilute-preview, :extend-preview, :buy-preview, :buy-suggest, :plan-batch, :probe-upload, :upload-file, :upload-collection, :feed-probe, :feed-timeline, :watch-ref, :watch-ref-stop, :pubsub-pss, :pubsub-gsoc, :pubsub-stop, :pubsub-filter, :pubsub-filter-clear, :pubsub-replay, :grantees-list, :hash, :cid, :depth-table, :gsoc-mine, :pss-target, :context, :quit)"
                )));
            }
        }
        Ok(())
    }

    /// Read-only "what would happen if I topped up batch X with N
    /// PLUR/chunk?". Pure math — no Bee calls, no writes. Args:
    /// `:topup-preview <batch-prefix> <amount-plur>`.
    fn run_topup_preview(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (prefix, amount_str) = match parts.as_slice() {
            [_, prefix, amount, ..] => (*prefix, *amount),
            _ => {
                return CommandStatus::Err(
                    "usage: :topup-preview <batch-prefix> <amount-plur-per-chunk>".into(),
                );
            }
        };
        let chain = match self.health_rx.borrow().chain_state.clone() {
            Some(c) => c,
            None => return CommandStatus::Err("chain state not loaded yet".into()),
        };
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        let amount = match stamp_preview::parse_plur_amount(amount_str) {
            Ok(a) => a,
            Err(e) => return CommandStatus::Err(e),
        };
        match stamp_preview::topup_preview(&batch, amount, &chain) {
            Ok(p) => CommandStatus::Info(p.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// `:dilute-preview <batch-prefix> <new-depth>` — pure math:
    /// halves per-chunk amount and TTL for each +1 in depth, doubles
    /// theoretical capacity.
    fn run_dilute_preview(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (prefix, depth_str) = match parts.as_slice() {
            [_, prefix, depth, ..] => (*prefix, *depth),
            _ => {
                return CommandStatus::Err(
                    "usage: :dilute-preview <batch-prefix> <new-depth>".into(),
                );
            }
        };
        let new_depth: u8 = match depth_str.parse() {
            Ok(d) => d,
            Err(_) => {
                return CommandStatus::Err(format!("invalid depth {depth_str:?} (expected u8)"));
            }
        };
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        match stamp_preview::dilute_preview(&batch, new_depth) {
            Ok(p) => CommandStatus::Info(p.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// `:extend-preview <batch-prefix> <duration>` — accepts `30d`,
    /// `12h`, `90m`, `45s`, or plain seconds.
    fn run_extend_preview(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (prefix, duration_str) = match parts.as_slice() {
            [_, prefix, duration, ..] => (*prefix, *duration),
            _ => {
                return CommandStatus::Err(
                    "usage: :extend-preview <batch-prefix> <duration>  (e.g. 30d, 12h, 90m, 45s, or plain seconds)".into(),
                );
            }
        };
        let extension_seconds = match stamp_preview::parse_duration_seconds(duration_str) {
            Ok(s) => s,
            Err(e) => return CommandStatus::Err(e),
        };
        let chain = match self.health_rx.borrow().chain_state.clone() {
            Some(c) => c,
            None => return CommandStatus::Err("chain state not loaded yet".into()),
        };
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        match stamp_preview::extend_preview(&batch, extension_seconds, &chain) {
            Ok(p) => CommandStatus::Info(p.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// `:probe-upload <batch-prefix>` — uploads one synthetic 4 KiB
    /// chunk to Bee and reports end-to-end latency. The cockpit is
    /// otherwise read-only; this is the deliberate exception. The
    /// chunk's payload is timestamp-randomised so each invocation
    /// fully exercises the upload + stamp path (no Bee dedup).
    ///
    /// Cost: one bucket increment on the chosen batch + the BZZ for
    /// one stamped chunk (`current_price` PLUR, fractions of a cent
    /// at typical prices). Returns immediately with a "started"
    /// notice; the actual outcome lands on the command bar via the
    /// async `cmd_status_tx` channel when Bee responds.
    fn run_probe_upload(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let prefix = match parts.as_slice() {
            [_, prefix, ..] => *prefix,
            _ => {
                return CommandStatus::Err(
                    "usage: :probe-upload <batch-prefix>  (uploads one synthetic 4 KiB chunk)"
                        .into(),
                );
            }
        };
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        if !batch.usable {
            return CommandStatus::Err(format!(
                "batch {} is not usable yet (waiting on chain confirmation) — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }
        if batch.batch_ttl <= 0 {
            return CommandStatus::Err(format!(
                "batch {} is expired — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }

        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let batch_id = batch.batch_id;
        let batch_short = short_hex(&batch.batch_id.to_hex(), 8);
        let task_short = batch_short.clone();
        tokio::spawn(async move {
            let chunk = build_synthetic_probe_chunk();
            let started = Instant::now();
            let result = api.bee().file().upload_chunk(&batch_id, chunk, None).await;
            let elapsed_ms = started.elapsed().as_millis();
            let status = match result {
                Ok(res) => CommandStatus::Info(format!(
                    "probe-upload OK in {elapsed_ms}ms — batch {task_short}, ref {}",
                    short_hex(&res.reference.to_hex(), 8),
                )),
                Err(e) => CommandStatus::Err(format!(
                    "probe-upload FAILED after {elapsed_ms}ms — batch {task_short}: {e}"
                )),
            };
            let _ = tx.send(status);
        });

        CommandStatus::Info(format!(
            "probe-upload to batch {batch_short} in flight — result will replace this line"
        ))
    }

    /// `:upload-file <path> <batch-prefix>` — upload a single local
    /// file via `POST /bzz` and return the resulting Swarm reference.
    /// Single-file scope only: directories error with a hint to use
    /// the (yet-to-ship) collection mode. The 256 MiB ceiling protects
    /// the cockpit from accidentally streaming a multi-GB file through
    /// the event loop; operators with bigger uploads should use
    /// swarm-cli where the upload runs out-of-process.
    fn run_upload_file(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (path_str, prefix) = match parts.as_slice() {
            [_, p, b, ..] => (*p, *b),
            _ => {
                return CommandStatus::Err("usage: :upload-file <path> <batch-prefix>".into());
            }
        };
        let path = std::path::PathBuf::from(path_str);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return CommandStatus::Err(format!("stat {path_str}: {e}")),
        };
        if meta.is_dir() {
            return CommandStatus::Err(format!(
                "{path_str} is a directory — :upload-file is single-file only (collection upload coming in a later release)"
            ));
        }
        const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
        if meta.len() > MAX_FILE_BYTES {
            return CommandStatus::Err(format!(
                "{path_str} is {} — over the {}-MiB cockpit ceiling; use swarm-cli for larger uploads",
                meta.len(),
                MAX_FILE_BYTES / (1024 * 1024),
            ));
        }
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        if !batch.usable {
            return CommandStatus::Err(format!(
                "batch {} is not usable yet (waiting on chain confirmation) — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }
        if batch.batch_ttl <= 0 {
            return CommandStatus::Err(format!(
                "batch {} is expired — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }

        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let batch_id = batch.batch_id;
        let batch_short = short_hex(&batch.batch_id.to_hex(), 8);
        let task_short = batch_short.clone();
        let file_size = meta.len();
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("")
            .to_string();
        let content_type = guess_content_type(&path);
        tokio::spawn(async move {
            let data = match tokio::fs::read(&path).await {
                Ok(b) => b,
                Err(e) => {
                    let _ = tx.send(CommandStatus::Err(format!("read {}: {e}", path.display())));
                    return;
                }
            };
            let started = Instant::now();
            let result = api
                .bee()
                .file()
                .upload_file(&batch_id, data, &name, &content_type, None)
                .await;
            let elapsed_ms = started.elapsed().as_millis();
            let status = match result {
                Ok(res) => CommandStatus::Info(format!(
                    "upload-file OK in {elapsed_ms}ms — {file_size}B → ref {} (batch {task_short})",
                    res.reference.to_hex(),
                )),
                Err(e) => CommandStatus::Err(format!(
                    "upload-file FAILED after {elapsed_ms}ms — batch {task_short}: {e}"
                )),
            };
            let _ = tx.send(status);
        });

        CommandStatus::Info(format!(
            "upload-file ({file_size}B) to batch {batch_short} in flight — result will replace this line"
        ))
    }

    /// `:upload-collection <dir> <batch-prefix>` — recursive
    /// directory upload via `POST /bzz` (tar). Hidden files /
    /// dirs (`.git`, `.env`, …) and symlinks are skipped; an
    /// `index.html` at the root auto-becomes the collection's
    /// default index. Caps: 256 MiB total, 10k entries — same
    /// reasoning as `:upload-file`'s 256-MiB single-file ceiling
    /// (keeps the cockpit's event loop responsive).
    fn run_upload_collection(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (dir_str, prefix) = match parts.as_slice() {
            [_, d, b, ..] => (*d, *b),
            _ => {
                return CommandStatus::Err("usage: :upload-collection <dir> <batch-prefix>".into());
            }
        };
        let dir = std::path::PathBuf::from(dir_str);
        let walked = match crate::uploads::walk_dir(&dir) {
            Ok(w) => w,
            Err(e) => return CommandStatus::Err(format!("walk {dir_str}: {e}")),
        };
        if walked.entries.is_empty() {
            return CommandStatus::Err(format!(
                "{dir_str} contains no uploadable files (after skipping hidden + symlinks)"
            ));
        }
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        if !batch.usable {
            return CommandStatus::Err(format!(
                "batch {} is not usable yet (waiting on chain confirmation) — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }
        if batch.batch_ttl <= 0 {
            return CommandStatus::Err(format!(
                "batch {} is expired — pick another",
                short_hex(&batch.batch_id.to_hex(), 8),
            ));
        }

        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let batch_id = batch.batch_id;
        let batch_short = short_hex(&batch.batch_id.to_hex(), 8);
        let task_short = batch_short.clone();
        let total_bytes = walked.total_bytes;
        let entry_count = walked.entries.len();
        let entries = walked.entries;
        let default_index = walked.default_index.clone();
        let dir_str_owned = dir_str.to_string();
        let default_index_for_msg = default_index.clone();
        tokio::spawn(async move {
            let opts = bee::api::CollectionUploadOptions {
                index_document: default_index,
                ..Default::default()
            };
            let started = Instant::now();
            let result = api
                .bee()
                .file()
                .upload_collection_entries(&batch_id, &entries, Some(&opts))
                .await;
            let elapsed_ms = started.elapsed().as_millis();
            let status = match result {
                Ok(res) => {
                    let idx = default_index_for_msg
                        .as_deref()
                        .map(|i| format!(" · index={i}"))
                        .unwrap_or_default();
                    CommandStatus::Info(format!(
                        "upload-collection OK in {elapsed_ms}ms — {entry_count} files, {total_bytes}B → ref {} (batch {task_short}){idx}",
                        res.reference.to_hex(),
                    ))
                }
                Err(e) => CommandStatus::Err(format!(
                    "upload-collection FAILED after {elapsed_ms}ms — {dir_str_owned} → batch {task_short}: {e}"
                )),
            };
            let _ = tx.send(status);
        });

        let idx_note = walked
            .default_index
            .as_deref()
            .map(|i| format!(" · default index={i}"))
            .unwrap_or_default();
        CommandStatus::Info(format!(
            "upload-collection {entry_count} files ({total_bytes}B){idx_note} to batch {batch_short} in flight — result will replace this line"
        ))
    }

    /// `:feed-probe <owner> <topic>` — fetch the latest update of a
    /// feed and surface its index, timestamp, and (when the payload
    /// is reference-shaped) the embedded Swarm reference. Async via
    /// `cmd_status_tx` because /feeds lookups can take 30-60s on a
    /// fresh feed (Bee's first lookup walks epoch indices).
    fn run_feed_probe(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (owner_str, topic_str) = match parts.as_slice() {
            [_, o, t, ..] => (*o, *t),
            _ => {
                return CommandStatus::Err(
                    "usage: :feed-probe <owner> <topic>  (topic = 64-hex or arbitrary string)"
                        .into(),
                );
            }
        };
        let parsed = match crate::feed_probe::parse_args(owner_str, topic_str) {
            Ok(p) => p,
            Err(e) => return CommandStatus::Err(e),
        };
        let owner_short = short_hex(&parsed.owner.to_hex(), 8);
        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            let started = Instant::now();
            let status = match crate::feed_probe::probe(api, parsed).await {
                Ok(r) => CommandStatus::Info(format!(
                    "{} ({}ms)",
                    r.summary(),
                    started.elapsed().as_millis()
                )),
                Err(e) => CommandStatus::Err(format!("feed-probe failed: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info(format!(
            "feed-probe owner={owner_short} in flight — result will replace this line (first lookup can take 30-60s)"
        ))
    }

    /// `:feed-timeline <owner> <topic> [N]` — walk the feed's
    /// history (newest first) and surface the entries on the S14
    /// screen. The walk runs async (10s of seconds for a fresh feed
    /// before the latest-index probe completes); the screen shows a
    /// spinner until the result lands. The optional `[N]` clamps the
    /// number of entries fetched (default 50, hard max 1000).
    fn run_feed_timeline(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (owner_str, topic_str, n_arg) = match parts.as_slice() {
            [_, o, t] => (*o, *t, None),
            [_, o, t, n, ..] => (*o, *t, Some(*n)),
            _ => {
                return CommandStatus::Err(
                    "usage: :feed-timeline <owner> <topic> [N]  (default 50, hard max 1000)".into(),
                );
            }
        };
        let parsed = match crate::feed_probe::parse_args(owner_str, topic_str) {
            Ok(p) => p,
            Err(e) => return CommandStatus::Err(e),
        };
        let max_entries = match n_arg {
            None => crate::feed_timeline::DEFAULT_MAX_ENTRIES,
            Some(s) => match s.parse::<u64>() {
                Ok(n) if n > 0 => n,
                _ => return CommandStatus::Err(format!("invalid N: {s:?}")),
            },
        };
        // Switch to S14 + reset the screen state synchronously so the
        // operator sees the spinner immediately. The result lands via
        // feed_timeline_rx on a future tick.
        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "FeedTimeline") {
            self.current_screen = idx;
            if let Some(ft) = self
                .screens
                .get_mut(idx)
                .and_then(|s| s.as_any_mut())
                .and_then(|a| a.downcast_mut::<FeedTimeline>())
            {
                let label = format!(
                    "owner=0x{} · topic={} · N={max_entries}",
                    short_hex(&parsed.owner.to_hex(), 8),
                    short_hex(&parsed.topic.to_hex(), 8),
                );
                ft.set_loading(label);
            }
        }
        let api = self.api.clone();
        let tx = self.feed_timeline_tx.clone();
        tokio::spawn(async move {
            let msg = match crate::feed_timeline::walk(api, parsed.owner, parsed.topic, max_entries)
                .await
            {
                Ok(t) => FeedTimelineMessage::Loaded(t),
                Err(e) => FeedTimelineMessage::Failed(e),
            };
            let _ = tx.send(msg);
        });
        CommandStatus::Info(format!(
            "feed-timeline N={max_entries} in flight — switching to S14 (first lookup can take 30-60s)"
        ))
    }

    /// `:hash <path>` — Swarm reference of a local file or directory,
    /// computed offline. Useful before paying for an upload to confirm
    /// the content's address-of-record matches what the dApp already
    /// committed to (the swarm-cli `hash` workflow).
    fn run_hash(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let path = match parts.as_slice() {
            [_, p, ..] => *p,
            _ => {
                return CommandStatus::Err(
                    "usage: :hash <path>  (file or directory; computed locally)".into(),
                );
            }
        };
        match utility_verbs::hash_path(path) {
            Ok(r) => CommandStatus::Info(format!("hash {path}: {r}")),
            Err(e) => CommandStatus::Err(format!("hash failed: {e}")),
        }
    }

    /// `:cid <ref> [manifest|feed]` — re-encode a 32-byte Swarm ref as
    /// a multibase CID string for ENS / IPFS-gateway integration. Kind
    /// defaults to manifest.
    fn run_cid(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (ref_hex, kind_arg) = match parts.as_slice() {
            [_, r, k, ..] => (*r, Some(*k)),
            [_, r] => (*r, None),
            _ => {
                return CommandStatus::Err(
                    "usage: :cid <ref> [manifest|feed]  (default manifest)".into(),
                );
            }
        };
        let kind = match utility_verbs::parse_cid_kind(kind_arg) {
            Ok(k) => k,
            Err(e) => return CommandStatus::Err(e),
        };
        match utility_verbs::cid_for_ref(ref_hex, kind) {
            Ok(cid) => CommandStatus::Info(format!("cid: {cid}")),
            Err(e) => CommandStatus::Err(format!("cid failed: {e}")),
        }
    }

    /// `:depth-table` — print the canonical depth → effective-bytes
    /// table the rest of the cockpit's economics math is anchored on.
    /// Result lands in the temp dir as a one-shot file because the
    /// command bar can't render an 18-row table.
    fn run_depth_table(&self) -> CommandStatus {
        let body = utility_verbs::depth_table();
        let path = std::env::temp_dir().join("bee-tui-depth-table.txt");
        match std::fs::write(&path, &body) {
            Ok(()) => CommandStatus::Info(format!("depth table → {}", path.display())),
            Err(e) => CommandStatus::Err(format!("depth-table write failed: {e}")),
        }
    }

    /// `:gsoc-mine <overlay> <identifier>` — pure CPU work that finds a
    /// `PrivateKey` whose SOC at `(identifier, owner)` lands close to
    /// the supplied overlay. Blocks the event loop briefly (≤ a few
    /// seconds typical) — acceptable for an interactive verb.
    fn run_gsoc_mine(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (overlay, ident) = match parts.as_slice() {
            [_, o, i, ..] => (*o, *i),
            _ => {
                return CommandStatus::Err(
                    "usage: :gsoc-mine <overlay-hex> <identifier>  (CPU work, no network)".into(),
                );
            }
        };
        match utility_verbs::gsoc_mine_for(overlay, ident) {
            Ok(out) => CommandStatus::Info(out.replace('\n', " · ")),
            Err(e) => CommandStatus::Err(format!("gsoc-mine failed: {e}")),
        }
    }

    /// `:manifest <ref>` — fetch the chunk + open S12 with a tree
    /// browser rooted on it. Async; the load lands on the screen via
    /// its own mpsc fetch channel, not via `cmd_status_tx`.
    fn run_manifest(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ref_arg = match parts.as_slice() {
            [_, r, ..] => *r,
            _ => {
                return CommandStatus::Err(
                    "usage: :manifest <ref>  (32-byte hex reference)".into(),
                );
            }
        };
        let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
            Ok(r) => r,
            Err(e) => return CommandStatus::Err(format!("manifest: bad ref: {e}")),
        };
        // Find the Manifest screen and ask it to load. Index lookup
        // by SCREEN_NAMES so future re-orders don't bit-rot.
        let idx = match SCREEN_NAMES.iter().position(|n| *n == "Manifest") {
            Some(i) => i,
            None => {
                return CommandStatus::Err("internal: Manifest screen not registered".into());
            }
        };
        let screen = self
            .screens
            .get_mut(idx)
            .and_then(|s| s.as_any_mut())
            .and_then(|a| a.downcast_mut::<Manifest>());
        let Some(manifest) = screen else {
            return CommandStatus::Err("internal: failed to access Manifest screen".into());
        };
        manifest.load(reference);
        self.current_screen = idx;
        CommandStatus::Info(format!("loading manifest {}", short_hex(ref_arg, 8)))
    }

    /// `:inspect <ref>` — universal "what is this thing?" verb.
    /// Fetches one chunk and tries `MantarayNode::unmarshal` to
    /// distinguish manifest from raw. On manifest, jumps to S12 with
    /// the tree opened; on raw, prints a one-line summary to the
    /// command-status row. Result delivered via the async cmd-status
    /// channel.
    fn run_inspect(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ref_arg = match parts.as_slice() {
            [_, r, ..] => *r,
            _ => {
                return CommandStatus::Err("usage: :inspect <ref>  (32-byte hex reference)".into());
            }
        };
        let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
            Ok(r) => r,
            Err(e) => return CommandStatus::Err(format!("inspect: bad ref: {e}")),
        };
        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let label = short_hex(ref_arg, 8);
        let label_for_task = label.clone();
        tokio::spawn(async move {
            let result = manifest_walker::inspect(api, reference).await;
            let status = match result {
                InspectResult::Manifest { node, bytes_len } => CommandStatus::Info(format!(
                    "inspect {label_for_task}: manifest · {bytes_len} bytes · {} forks (jump to :manifest {label_for_task})",
                    node.forks.len(),
                )),
                InspectResult::RawChunk { bytes_len } => CommandStatus::Info(format!(
                    "inspect {label_for_task}: raw chunk · {bytes_len} bytes · not a manifest"
                )),
                InspectResult::Error(e) => {
                    CommandStatus::Err(format!("inspect {label_for_task} failed: {e}"))
                }
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info(format!(
            "inspecting {label} — result will replace this line"
        ))
    }

    /// `:durability-check <ref>` — walk the chunk graph rooted at
    /// `<ref>` and record the result on the S13 Watchlist screen.
    /// Async; the immediate command-status shows "in flight", the
    /// final summary lands when the walk completes.
    ///
    /// On manifest references the walk is recursive (root + every
    /// fork's `self_address`); on raw chunks it's just the single
    /// fetch. Either way, the cockpit jumps to S13 so the operator
    /// sees the running history while the new check completes.
    /// `:grantees-list <ref>` — fetch `GET /grantee/{ref}` and print
    /// the registered public key list. Read-only; pairs cleanly with
    /// `:inspect`. A full S16 ACT Grantees screen with create/patch
    /// is on the v1.8+ roadmap; this verb is the read-side
    /// foundation operators need today.
    fn run_grantees_list(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ref_arg = match parts.as_slice() {
            [_, r, ..] => *r,
            _ => return CommandStatus::Err("usage: :grantees-list <ref>".into()),
        };
        let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
            Ok(r) => r,
            Err(e) => return CommandStatus::Err(format!("grantees-list: bad ref: {e}")),
        };
        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let label = short_hex(ref_arg, 8);
        let label_for_task = label.clone();
        tokio::spawn(async move {
            let status = match api.bee().api().get_grantees(&reference).await {
                Ok(list) => {
                    if list.is_empty() {
                        CommandStatus::Info(format!(
                            "grantees-list {label_for_task}: no grantees registered"
                        ))
                    } else {
                        let preview: Vec<String> =
                            list.iter().take(3).map(|p| short_hex(p, 12)).collect();
                        let suffix = if list.len() > 3 {
                            format!(" (+{} more)", list.len() - 3)
                        } else {
                            String::new()
                        };
                        CommandStatus::Info(format!(
                            "grantees-list {label_for_task}: {} grantee(s) — {}{suffix}",
                            list.len(),
                            preview.join(", "),
                        ))
                    }
                }
                Err(e) => CommandStatus::Err(format!("grantees-list {label_for_task} failed: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info(format!(
            "grantees-list {label} in flight — result will replace this line"
        ))
    }

    fn run_durability_check(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let ref_arg = match parts.as_slice() {
            [_, r, ..] => *r,
            _ => {
                return CommandStatus::Err(
                    "usage: :durability-check <ref>  (32-byte hex reference)".into(),
                );
            }
        };
        let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
            Ok(r) => r,
            Err(e) => {
                return CommandStatus::Err(format!("durability-check: bad ref: {e}"));
            }
        };
        // Jump to S13 so the operator sees the existing history while
        // the new walk completes.
        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Watchlist") {
            self.current_screen = idx;
        }
        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        let watchlist_tx = self.durability_tx.clone();
        let label = short_hex(ref_arg, 8);
        let label_for_task = label.clone();
        let opts = self.durability_check_options();
        tokio::spawn(async move {
            let result = durability::check_with_options(api, reference, opts).await;
            let summary = result.summary();
            let _ = watchlist_tx.send(result);
            let _ = tx.send(if summary.contains("UNHEALTHY") {
                CommandStatus::Err(summary)
            } else {
                CommandStatus::Info(summary)
            });
        });
        CommandStatus::Info(format!(
            "durability-check {label_for_task} in flight — see S13 Watchlist for the running history"
        ))
    }

    /// Read `[durability]` from config and convert to a
    /// `CheckOptions`. Cheap; called per-walk so config edits picked
    /// up via a future `:context` switch take effect on the next
    /// check without a cockpit restart.
    fn durability_check_options(&self) -> durability::CheckOptions {
        durability::CheckOptions {
            bmt_verify: true,
            swarmscan_url: if self.config.durability.swarmscan_check {
                Some(self.config.durability.swarmscan_url.clone())
            } else {
                None
            },
        }
    }

    /// `:watch-ref <ref> [interval-secs]` — start a daemon loop that
    /// runs `:durability-check` on `<ref>` every `interval-secs`
    /// seconds. Each run's result lands in S13 Watchlist via the
    /// existing `durability_tx` channel. The cockpit's `root_cancel`
    /// triggers shutdown on quit; `:watch-ref-stop [ref]` triggers
    /// shutdown earlier. Re-issuing `:watch-ref` for a ref already
    /// being watched cancels the prior daemon and starts a fresh one
    /// (so the operator can change the interval without an explicit
    /// stop).
    fn run_watch_ref(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (ref_arg, interval_arg) = match parts.as_slice() {
            [_, r] => (*r, None),
            [_, r, i, ..] => (*r, Some(*i)),
            _ => {
                return CommandStatus::Err(
                    "usage: :watch-ref <ref> [interval-secs]  (default 60s)".into(),
                );
            }
        };
        let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
            Ok(r) => r,
            Err(e) => return CommandStatus::Err(format!("watch-ref: bad ref: {e}")),
        };
        let interval_secs = match interval_arg {
            None => 60u64,
            Some(s) => match s.parse::<u64>() {
                Ok(n) if (10..=86_400).contains(&n) => n,
                Ok(n) => {
                    return CommandStatus::Err(format!(
                        "watch-ref: interval {n}s out of range (10..=86400)"
                    ));
                }
                Err(_) => return CommandStatus::Err(format!("watch-ref: invalid interval: {s:?}")),
            },
        };
        let key = reference.to_hex();
        // If a daemon is already running for this ref, cancel it
        // first so we don't double-fire checks.
        if let Some(prev) = self.watch_refs.remove(&key) {
            prev.cancel();
        }
        let cancel = self.root_cancel.child_token();
        self.watch_refs.insert(key.clone(), cancel.clone());

        let api = self.api.clone();
        let watchlist_tx = self.durability_tx.clone();
        let label = short_hex(ref_arg, 8);
        let label_for_task = label.clone();
        let opts = self.durability_check_options();
        tokio::spawn(async move {
            let interval = std::time::Duration::from_secs(interval_secs);
            loop {
                let result =
                    durability::check_with_options(api.clone(), reference.clone(), opts.clone())
                        .await;
                let _ = watchlist_tx.send(result);
                tokio::select! {
                    _ = tokio::time::sleep(interval) => {}
                    _ = cancel.cancelled() => return,
                }
            }
        });

        CommandStatus::Info(format!(
            "watch-ref {label_for_task} started — re-checking every {interval_secs}s; results in S13 Watchlist"
        ))
    }

    /// `:watch-ref-stop [ref]` — cancel a running `:watch-ref`
    /// daemon. With no arg, cancels every active daemon; with a
    /// `<ref>` arg, cancels only the matching one. The daemon's
    /// tokio task observes the cancel on its next iteration
    /// boundary (i.e. up to `interval-secs` later); the App's
    /// hashmap entry is removed immediately.
    fn run_watch_ref_stop(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            [_] => {
                let n = self.watch_refs.len();
                for (_, c) in self.watch_refs.drain() {
                    c.cancel();
                }
                CommandStatus::Info(format!("watch-ref-stop: cancelled {n} active daemon(s)"))
            }
            [_, r, ..] => {
                let reference = match bee::swarm::Reference::from_hex(r.trim()) {
                    Ok(r) => r,
                    Err(e) => return CommandStatus::Err(format!("watch-ref-stop: bad ref: {e}")),
                };
                let key = reference.to_hex();
                match self.watch_refs.remove(&key) {
                    Some(c) => {
                        c.cancel();
                        CommandStatus::Info(format!(
                            "watch-ref-stop: cancelled daemon for {}",
                            short_hex(r, 8)
                        ))
                    }
                    None => CommandStatus::Err(format!(
                        "watch-ref-stop: no daemon running for {}",
                        short_hex(r, 8)
                    )),
                }
            }
            _ => CommandStatus::Err("usage: :watch-ref-stop [ref]  (omit ref to stop all)".into()),
        }
    }

    /// `:pubsub-pss <topic>` — open a PSS subscription on `<topic>`
    /// and surface every received message on the S15 Pubsub screen.
    /// Topic accepts the same forms as `:feed-probe`: 64-hex literal
    /// or arbitrary string (keccak256-hashed via
    /// `Topic::from_string`). Re-issuing for an already-watched
    /// topic refuses with a clear error so duplicate sockets don't
    /// silently pile up — operator must `:pubsub-stop <sub-id>` first.
    fn run_pubsub_pss(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let topic_str = match parts.as_slice() {
            [_, t, ..] => *t,
            _ => return CommandStatus::Err("usage: :pubsub-pss <topic>".into()),
        };
        // Reuse :feed-probe's topic parser (string OR 64-hex literal).
        let parsed = match crate::feed_probe::parse_args(
            "0x0000000000000000000000000000000000000000",
            topic_str,
        ) {
            Ok(p) => p,
            Err(e) => return CommandStatus::Err(format!("pubsub-pss: {e}")),
        };
        let topic = parsed.topic;
        let sub_id = crate::pubsub::pss_sub_id(&topic);
        if self.pubsub_subs.contains_key(&sub_id) {
            return CommandStatus::Err(format!(
                "pubsub-pss: already subscribed to {sub_id} (use :pubsub-stop {sub_id} first)"
            ));
        }
        let cancel = self.root_cancel.child_token();
        self.pubsub_subs.insert(sub_id.clone(), cancel.clone());
        self.jump_to_pubsub_screen();
        let api = self.api.clone();
        let tx = self.pubsub_msg_tx.clone();
        let status_tx = self.cmd_status_tx.clone();
        let sub_id_for_task = sub_id.clone();
        let history = self.pubsub_history.clone();
        tokio::spawn(async move {
            if let Err(e) = crate::pubsub::spawn_pss_watcher(api, topic, cancel, tx, history).await
            {
                let _ = status_tx.send(CommandStatus::Err(format!(
                    "pubsub-pss {sub_id_for_task}: {e}"
                )));
            }
        });
        CommandStatus::Info(format!("pubsub-pss subscribed: {sub_id}"))
    }

    /// `:pubsub-gsoc <owner> <identifier>` — open a GSOC subscription
    /// on the SOC keyed by `(owner, identifier)`. Both args accept
    /// `0x`-prefixed or bare hex (40 chars for owner, 64 chars for
    /// identifier).
    fn run_pubsub_gsoc(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (owner_str, id_str) = match parts.as_slice() {
            [_, o, i, ..] => (*o, *i),
            _ => return CommandStatus::Err("usage: :pubsub-gsoc <owner> <identifier>".into()),
        };
        let owner = match bee::swarm::EthAddress::from_hex(owner_str.trim()) {
            Ok(o) => o,
            Err(e) => return CommandStatus::Err(format!("pubsub-gsoc: bad owner: {e}")),
        };
        let identifier = match bee::swarm::Identifier::from_hex(id_str.trim()) {
            Ok(i) => i,
            Err(e) => return CommandStatus::Err(format!("pubsub-gsoc: bad identifier: {e}")),
        };
        let sub_id = crate::pubsub::gsoc_sub_id(&owner, &identifier);
        if self.pubsub_subs.contains_key(&sub_id) {
            return CommandStatus::Err(format!(
                "pubsub-gsoc: already subscribed to {sub_id} (use :pubsub-stop first)"
            ));
        }
        let cancel = self.root_cancel.child_token();
        self.pubsub_subs.insert(sub_id.clone(), cancel.clone());
        self.jump_to_pubsub_screen();
        let api = self.api.clone();
        let tx = self.pubsub_msg_tx.clone();
        let status_tx = self.cmd_status_tx.clone();
        let sub_id_for_task = sub_id.clone();
        let history = self.pubsub_history.clone();
        tokio::spawn(async move {
            if let Err(e) =
                crate::pubsub::spawn_gsoc_watcher(api, owner, identifier, cancel, tx, history).await
            {
                let _ = status_tx.send(CommandStatus::Err(format!(
                    "pubsub-gsoc {sub_id_for_task}: {e}"
                )));
            }
        });
        CommandStatus::Info(format!("pubsub-gsoc subscribed: {sub_id}"))
    }

    /// `:pubsub-stop [sub-id]` — cancel pubsub subscriptions. With
    /// no arg, cancels every active subscription; with a `<sub-id>`
    /// arg (`pss:...` or `gsoc:...`), cancels just that one.
    fn run_pubsub_stop(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        match parts.as_slice() {
            [_] => {
                let n = self.pubsub_subs.len();
                for (_, c) in self.pubsub_subs.drain() {
                    c.cancel();
                }
                CommandStatus::Info(format!("pubsub-stop: cancelled {n} subscription(s)"))
            }
            [_, id, ..] => match self.pubsub_subs.remove(*id) {
                Some(c) => {
                    c.cancel();
                    CommandStatus::Info(format!("pubsub-stop: cancelled {id}"))
                }
                None => CommandStatus::Err(format!("pubsub-stop: no active subscription {id}")),
            },
            _ => CommandStatus::Err("usage: :pubsub-stop [sub-id]".into()),
        }
    }

    /// Helper used by :pubsub-pss / :pubsub-gsoc to jump to S15 so
    /// the operator sees their incoming messages immediately.
    fn jump_to_pubsub_screen(&mut self) {
        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Pubsub") {
            self.current_screen = idx;
        }
    }

    /// `:pubsub-filter <substring>` — show only S15 rows whose
    /// channel hex or smart-preview contains the given substring.
    /// Case-insensitive; underlying ring still receives every
    /// message (filtering is presentation-only).
    fn run_pubsub_filter(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.splitn(2, char::is_whitespace).collect();
        let needle = match parts.as_slice() {
            [_, rest] => rest.trim().to_string(),
            _ => return CommandStatus::Err("usage: :pubsub-filter <substring>".into()),
        };
        if needle.is_empty() {
            return CommandStatus::Err("usage: :pubsub-filter <substring>".into());
        }
        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Pubsub") {
            if let Some(ps) = self
                .screens
                .get_mut(idx)
                .and_then(|s| s.as_any_mut())
                .and_then(|a| a.downcast_mut::<Pubsub>())
            {
                ps.set_filter(Some(needle.clone()));
            }
            self.current_screen = idx;
        }
        CommandStatus::Info(format!("pubsub-filter: showing rows containing {needle:?}"))
    }

    /// `:pubsub-filter-clear` — remove the active S15 filter.
    fn run_pubsub_filter_clear(&mut self) -> CommandStatus {
        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Pubsub") {
            if let Some(ps) = self
                .screens
                .get_mut(idx)
                .and_then(|s| s.as_any_mut())
                .and_then(|a| a.downcast_mut::<Pubsub>())
            {
                ps.set_filter(None);
            }
        }
        CommandStatus::Info("pubsub-filter-clear: filter removed".into())
    }

    /// `:pubsub-replay <path>` — load a previously-written pubsub
    /// history JSONL file and push its messages onto the S15 timeline
    /// so the operator can browse a past session without an active
    /// subscription. Caps at MAX_MESSAGES; bad lines are skipped with
    /// a warn log. Replay does not start any watchers.
    fn run_pubsub_replay(&mut self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let path_str = match parts.as_slice() {
            [_, p, ..] => *p,
            _ => return CommandStatus::Err("usage: :pubsub-replay <path>".into()),
        };
        let path = std::path::PathBuf::from(path_str);
        self.jump_to_pubsub_screen();
        let tx = self.pubsub_msg_tx.clone();
        let status_tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            match crate::pubsub::replay_history_file(&path).await {
                Ok(msgs) => {
                    let n = msgs.len();
                    // Push oldest → newest so record() ends with newest at front.
                    for m in msgs {
                        let _ = tx.send(m);
                    }
                    let _ = status_tx.send(CommandStatus::Info(format!(
                        "pubsub-replay: loaded {n} message(s)"
                    )));
                }
                Err(e) => {
                    let _ = status_tx.send(CommandStatus::Err(format!("pubsub-replay: {e}")));
                }
            }
        });
        CommandStatus::Info(format!("pubsub-replay: loading {path_str}…"))
    }

    /// `:pss-target <overlay>` — Bee's `/pss/send` accepts at most a
    /// 4-hex-char target prefix. This verb extracts those four chars
    /// from a full overlay so dApp authors don't have to re-derive
    /// the rule.
    fn run_pss_target(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let overlay = match parts.as_slice() {
            [_, o, ..] => *o,
            _ => {
                return CommandStatus::Err(
                    "usage: :pss-target <overlay-hex>  (returns first 4 hex chars)".into(),
                );
            }
        };
        match utility_verbs::pss_target_for(overlay) {
            Ok(prefix) => CommandStatus::Info(format!("pss target prefix: {prefix}")),
            Err(e) => CommandStatus::Err(format!("pss-target failed: {e}")),
        }
    }

    /// `:price` — fire a one-shot fetch of the xBZZ → USD spot
    /// price from Swarm's public tokenservice. Async via
    /// cmd_status_tx. The cockpit doesn't auto-poll the price —
    /// operators ask for it when they want to think about
    /// economics in dollars.
    fn run_price(&self) -> CommandStatus {
        let tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            let status = match economics_oracle::fetch_xbzz_price().await {
                Ok(p) => CommandStatus::Info(p.summary()),
                Err(e) => CommandStatus::Err(format!("price: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info("price: querying tokenservice.ethswarm.org…".into())
    }

    /// `:basefee` — fire JSON-RPC calls against the configured
    /// Gnosis RPC endpoint (`[economics].gnosis_rpc_url`) for the
    /// pending block's basefee + the network's expected tip. Async.
    fn run_basefee(&self) -> CommandStatus {
        let url = match self.config.economics.gnosis_rpc_url.clone() {
            Some(u) => u,
            None => {
                return CommandStatus::Err(
                    "basefee: set [economics].gnosis_rpc_url in config.toml (typically the same URL as Bee's --blockchain-rpc-endpoint)"
                        .into(),
                );
            }
        };
        let tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            let status = match economics_oracle::fetch_gnosis_gas(&url).await {
                Ok(g) => CommandStatus::Info(g.summary()),
                Err(e) => CommandStatus::Err(format!("basefee: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info("basefee: querying gnosis RPC…".into())
    }

    /// `:config-doctor` — audit the operator's `bee.yaml` against
    /// the deprecation list ported from swarm-desktop's
    /// `migration.ts`. Read-only — the cockpit never modifies the
    /// operator's config. Report lands as a temp file the operator
    /// can review and apply by hand.
    fn run_config_doctor(&self) -> CommandStatus {
        let path = match self.config.bee.as_ref().map(|b| b.config.clone()) {
            Some(p) => p,
            None => {
                return CommandStatus::Err(
                    "config-doctor: no [bee].config in config.toml (or pass --bee-config) — point bee-tui at the bee.yaml you want audited"
                        .into(),
                );
            }
        };
        let report = match config_doctor::audit(&path) {
            Ok(r) => r,
            Err(e) => return CommandStatus::Err(format!("config-doctor: {e}")),
        };
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let out_path = std::env::temp_dir().join(format!("bee-tui-config-doctor-{secs}.txt"));
        if let Err(e) = std::fs::write(&out_path, report.render()) {
            return CommandStatus::Err(format!("config-doctor write {}: {e}", out_path.display()));
        }
        CommandStatus::Info(format!("{} → {}", report.summary(), out_path.display()))
    }

    /// `:check-version` — fire a GitHub `releases/latest` lookup for
    /// `ethersphere/bee` and pair the result with the version the
    /// local Bee reported on `/health`. Both fetches happen in the
    /// spawned task (`/health` for the running version, GitHub for
    /// the latest); the watch hub's `HealthSnapshot` carries
    /// `/status` data, not the structured Bee version, so we hit
    /// `/health` explicitly here.
    fn run_check_version(&self) -> CommandStatus {
        let api = self.api.clone();
        let tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            let running = api.bee().debug().health().await.ok().map(|h| h.version);
            let status = match version_check::check_latest(running).await {
                Ok(v) => CommandStatus::Info(v.summary()),
                Err(e) => CommandStatus::Err(format!("check-version failed: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info("check-version: querying github.com/ethersphere/bee…".into())
    }

    /// `:plan-batch <batch-prefix> [usage-thr] [ttl-thr] [extra-depth]` —
    /// runs beekeeper-stamper's `Set` algorithm read-only and tells
    /// the operator whether the batch needs topup, dilute, both, or
    /// nothing — plus the BZZ cost. Defaults: usage 0.85, TTL 24h,
    /// extra depth +2 (cross-ecosystem convention).
    fn run_plan_batch(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let prefix = match parts.as_slice() {
            [_, prefix, ..] => *prefix,
            _ => {
                return CommandStatus::Err(
                    "usage: :plan-batch <batch-prefix> [usage-thr] [ttl-thr] [extra-depth]".into(),
                );
            }
        };
        let usage_thr = match parts.get(2) {
            Some(s) => match s.parse::<f64>() {
                Ok(v) => v,
                Err(_) => {
                    return CommandStatus::Err(format!(
                        "invalid usage-thr {s:?} (expected float in [0,1], default 0.85)"
                    ));
                }
            },
            None => stamp_preview::DEFAULT_USAGE_THRESHOLD,
        };
        let ttl_thr = match parts.get(3) {
            Some(s) => match stamp_preview::parse_duration_seconds(s) {
                Ok(v) => v,
                Err(e) => return CommandStatus::Err(format!("ttl-thr: {e}")),
            },
            None => stamp_preview::DEFAULT_TTL_THRESHOLD_SECONDS,
        };
        let extra_depth = match parts.get(4) {
            Some(s) => match s.parse::<u8>() {
                Ok(v) => v,
                Err(_) => {
                    return CommandStatus::Err(format!(
                        "invalid extra-depth {s:?} (expected u8, default 2)"
                    ));
                }
            },
            None => stamp_preview::DEFAULT_EXTRA_DEPTH,
        };
        let chain = match self.health_rx.borrow().chain_state.clone() {
            Some(c) => c,
            None => return CommandStatus::Err("chain state not loaded yet".into()),
        };
        let stamps = self.watch.stamps().borrow().clone();
        let batch = match stamp_preview::match_batch_prefix(&stamps.batches, prefix) {
            Ok(b) => b.clone(),
            Err(e) => return CommandStatus::Err(e),
        };
        match stamp_preview::plan_batch(&batch, &chain, usage_thr, ttl_thr, extra_depth) {
            Ok(p) => CommandStatus::Info(p.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// `:buy-suggest <size> <duration>` — inverse of buy-preview.
    /// Operator says "I want X bytes for Y seconds", we return the
    /// minimum `(depth, amount)` that covers it. Depth rounds up
    /// to the next power of two so the headroom is operator-visible;
    /// duration rounds up in chain blocks.
    fn run_buy_suggest(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (size_str, duration_str) = match parts.as_slice() {
            [_, size, duration, ..] => (*size, *duration),
            _ => {
                return CommandStatus::Err(
                    "usage: :buy-suggest <size> <duration>  (e.g. 5GiB 30d, 100MiB 12h)".into(),
                );
            }
        };
        let target_bytes = match stamp_preview::parse_size_bytes(size_str) {
            Ok(b) => b,
            Err(e) => return CommandStatus::Err(e),
        };
        let target_seconds = match stamp_preview::parse_duration_seconds(duration_str) {
            Ok(s) => s,
            Err(e) => return CommandStatus::Err(e),
        };
        let chain = match self.health_rx.borrow().chain_state.clone() {
            Some(c) => c,
            None => return CommandStatus::Err("chain state not loaded yet".into()),
        };
        match stamp_preview::buy_suggest(target_bytes, target_seconds, &chain) {
            Ok(s) => CommandStatus::Info(s.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// `:buy-preview <depth> <amount-plur>` — hypothetical fresh
    /// batch; no batch lookup needed.
    fn run_buy_preview(&self, line: &str) -> CommandStatus {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let (depth_str, amount_str) = match parts.as_slice() {
            [_, depth, amount, ..] => (*depth, *amount),
            _ => {
                return CommandStatus::Err(
                    "usage: :buy-preview <depth> <amount-plur-per-chunk>".into(),
                );
            }
        };
        let depth: u8 = match depth_str.parse() {
            Ok(d) => d,
            Err(_) => {
                return CommandStatus::Err(format!("invalid depth {depth_str:?} (expected u8)"));
            }
        };
        let amount = match stamp_preview::parse_plur_amount(amount_str) {
            Ok(a) => a,
            Err(e) => return CommandStatus::Err(e),
        };
        let chain = match self.health_rx.borrow().chain_state.clone() {
            Some(c) => c,
            None => return CommandStatus::Err("chain state not loaded yet".into()),
        };
        match stamp_preview::buy_preview(depth, amount, &chain) {
            Ok(p) => CommandStatus::Info(p.summary()),
            Err(e) => CommandStatus::Err(e),
        }
    }

    /// Tear down the current watch hub and ApiClient, build a new
    /// connection against the named NodeConfig, and rebuild the
    /// screen list against fresh receivers. Component-internal state
    /// (Lottery's bench history, Network's reachability stability
    /// timer, etc.) is intentionally lost — a profile switch is a
    /// fresh slate, the same way it would be on app restart.
    fn switch_context(&mut self, target: &str) -> color_eyre::Result<()> {
        let node = self
            .config
            .nodes
            .iter()
            .find(|n| n.name == target)
            .ok_or_else(|| eyre!("no node configured with name {target:?}"))?
            .clone();
        let new_api = Arc::new(ApiClient::from_node(&node)?);
        // Cancel any pubsub subscriptions + watch-ref daemons spawned
        // against the previous node. Their tokio tasks each hold an
        // `Arc<ApiClient>` to the *old* node — without cancelling them
        // here they would keep polling the wrong URL and leak old-node
        // messages into the new screens. Operators that want them on
        // the new node must re-issue the verbs after the switch.
        for (_, c) in self.pubsub_subs.drain() {
            c.cancel();
        }
        for (_, c) in self.watch_refs.drain() {
            c.cancel();
        }
        // Reset gate-state memory: the old node's `Pass`/`Fail` history
        // is meaningless for the new node, and a stale entry could
        // fire a spurious webhook on the next tick (or suppress a
        // genuine transition because the old status happened to match).
        self.alert_state = crate::alerts::AlertState::new(self.config.alerts.debounce_secs);
        // Cancel the current hub's children and let it drop. The new
        // hub spawns under the same root_cancel so quit-time teardown
        // still walks the whole tree in one go.
        self.watch.shutdown();
        let refresh = RefreshProfile::from_config(&self.config.ui.refresh);
        let new_watch = BeeWatch::start_with_profile(new_api.clone(), &self.root_cancel, refresh);
        let new_health_rx = new_watch.health();
        // Spin up a fresh cost-context poller for the new context;
        // the old one keeps emitting until root_cancel fires at quit
        // (cheap — one tokio task), but the screens consume only the
        // new receiver after this point.
        let new_market_rx = if self.config.economics.enable_market_tile {
            Some(economics_oracle::spawn_poller(
                self.config.economics.gnosis_rpc_url.clone(),
                self.root_cancel.child_token(),
            ))
        } else {
            None
        };
        let new_screens = build_screens(
            &new_api,
            &new_watch,
            new_market_rx,
            self.fleet_rx.clone(),
            self.fleet_resync_tx.clone(),
        );
        self.api = new_api;
        self.watch = new_watch;
        self.health_rx = new_health_rx;
        self.screens = new_screens;
        // Re-point the external bee-log tailer at the new node's log
        // source. Skipped entirely when bee-tui owns the supervisor:
        // the supervised child's log stays relevant no matter which
        // profile the operator is viewing, so its tailer is left
        // running. In external mode: cancel the old node's tailer,
        // then resolve the new node's source — explicit
        // `log_command` / `log_file` config first, then `/proc`
        // auto-discovery — and spawn a fresh tailer (or record the
        // "can't capture" hint). The `--bee-log*` CLI overrides are
        // startup-only knobs, so `None` is passed for the CLI tier.
        if self.supervisor.is_none() {
            if let Some(c) = self.bee_log_tailer_cancel.take() {
                c.cancel();
            }
            let resolved = match resolve_bee_log_source(
                None,
                None,
                node.log_command.as_deref(),
                node.log_file.as_deref(),
            ) {
                Some(src) => DiscoveryResult::Found(src),
                None => bee_log_discover::discover(&self.api.url),
            };
            let hint = match resolved {
                DiscoveryResult::Found(source) => {
                    let (rx, cancel) = spawn_bee_log_tailer(source, &self.root_cancel);
                    self.bee_log_rx = Some(rx);
                    self.bee_log_tailer_cancel = Some(cancel);
                    None
                }
                DiscoveryResult::Unsupported(msg) => {
                    self.bee_log_rx = None;
                    Some(msg)
                }
                DiscoveryResult::NotApplicable => {
                    self.bee_log_rx = None;
                    None
                }
            };
            self.log_pane
                .set_spawn_active(self.bee_log_tailer_cancel.is_some());
            self.log_pane.set_log_source_hint(hint);
        }
        // Keep the same tab index so the operator stays on the
        // screen they were looking at — same data shape, new node.
        Ok(())
    }

    /// Build and persist a redacted diagnostic bundle to a file in
    /// the system temp directory. Designed to be paste-ready into a
    /// support thread (Discord, GitHub issue) without leaking
    /// auth tokens — URLs are reduced to their path component, since
    /// Bearer tokens live in headers, not URLs.
    /// Kick off `GET /pins/check` in a background task. Returns the
    /// destination file path immediately so the operator can `tail -f`
    /// it while bee-rs streams the NDJSON response. Each pin is
    /// appended as a single line: `<ref>  total=N  missing=N  invalid=N
    /// (healthy|UNHEALTHY)`. A `# done. <n> pins checked.` trailer
    /// signals completion.
    ///
    /// The task captures `Arc<ApiClient>` so a `:context` switch
    /// mid-check still completes against the original profile — the
    /// destination file's name pins the profile so two parallel
    /// invocations against different profiles don't collide.
    fn start_pins_check(&self) -> std::io::Result<PathBuf> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "bee-tui-pins-check-{}-{secs}.txt",
            sanitize_for_filename(&self.api.name),
        ));
        // Pre-create with a header so the operator's `tail -f` finds
        // something immediately, even before the first pin lands.
        std::fs::write(
            &path,
            format!(
                "# bee-tui :pins-check\n# profile  {}\n# endpoint {}\n# started  {}\n",
                self.api.name,
                self.api.url,
                format_utc_now(),
            ),
        )?;

        let api = self.api.clone();
        let dest = path.clone();
        tokio::spawn(async move {
            let bee = api.bee();
            match bee.api().check_pins(None).await {
                Ok(entries) => {
                    let mut body = String::new();
                    for e in &entries {
                        body.push_str(&format!(
                            "{}  total={}  missing={}  invalid={}  {}\n",
                            e.reference.to_hex(),
                            e.total,
                            e.missing,
                            e.invalid,
                            if e.is_healthy() {
                                "healthy"
                            } else {
                                "UNHEALTHY"
                            },
                        ));
                    }
                    body.push_str(&format!("# done. {} pins checked.\n", entries.len()));
                    if let Err(e) = append(&dest, &body) {
                        let _ = append(&dest, &format!("# write error: {e}\n"));
                    }
                }
                Err(e) => {
                    let _ = append(&dest, &format!("# error: {e}\n"));
                }
            }
        });
        Ok(path)
    }

    /// Spawn a fire-and-forget task that calls
    /// `set_logger(expression, level)` against the node. The result
    /// (success or error) is appended to a `:loggers`-style log file
    /// so the operator has a paper trail of mutations made from the
    /// cockpit. Per-profile and per-call so multiple `:set-logger`
    /// invocations don't trample each other's record.
    ///
    /// Bee will validate `level` against its own enum (`none|error|
    /// warning|info|debug|all`); bee-rs does the same client-side, so
    /// a mistyped level errors out before any HTTP request goes out.
    fn start_set_logger(&self, expression: String, level: String) {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dest = std::env::temp_dir().join(format!(
            "bee-tui-set-logger-{}-{secs}.txt",
            sanitize_for_filename(&self.api.name),
        ));
        let _ = std::fs::write(
            &dest,
            format!(
                "# bee-tui :set-logger\n# profile  {}\n# endpoint {}\n# expr     {expression}\n# level    {level}\n# started  {}\n",
                self.api.name,
                self.api.url,
                format_utc_now(),
            ),
        );

        let api = self.api.clone();
        tokio::spawn(async move {
            let bee = api.bee();
            match bee.debug().set_logger(&expression, &level).await {
                Ok(()) => {
                    let _ = append(
                        &dest,
                        &format!("# done. {expression} → {level} accepted by Bee.\n"),
                    );
                }
                Err(e) => {
                    let _ = append(&dest, &format!("# error: {e}\n"));
                }
            }
        });
    }

    /// Snapshot Bee's logger configuration to a file. Same on-demand
    /// pattern as `:pins-check`: capture the registered loggers + their
    /// verbosity into a sortable text table so operators can answer
    /// "is push-sync at debug right now?" without curling the API.
    fn start_loggers_dump(&self) -> std::io::Result<PathBuf> {
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "bee-tui-loggers-{}-{secs}.txt",
            sanitize_for_filename(&self.api.name),
        ));
        std::fs::write(
            &path,
            format!(
                "# bee-tui :loggers\n# profile  {}\n# endpoint {}\n# started  {}\n",
                self.api.name,
                self.api.url,
                format_utc_now(),
            ),
        )?;

        let api = self.api.clone();
        let dest = path.clone();
        tokio::spawn(async move {
            let bee = api.bee();
            match bee.debug().loggers().await {
                Ok(listing) => {
                    let mut rows = listing.loggers.clone();
                    // Stable sort: verbosity buckets first ("all"
                    // before "1"/"info" etc. so the loud loggers
                    // float to the top), then logger name.
                    rows.sort_by(|a, b| {
                        verbosity_rank(&b.verbosity)
                            .cmp(&verbosity_rank(&a.verbosity))
                            .then_with(|| a.logger.cmp(&b.logger))
                    });
                    let mut body = String::new();
                    body.push_str(&format!("# {} loggers registered\n", rows.len()));
                    body.push_str("# VERBOSITY  LOGGER\n");
                    for r in &rows {
                        body.push_str(&format!("  {:<9}  {}\n", r.verbosity, r.logger,));
                    }
                    body.push_str("# done.\n");
                    if let Err(e) = append(&dest, &body) {
                        let _ = append(&dest, &format!("# write error: {e}\n"));
                    }
                }
                Err(e) => {
                    let _ = append(&dest, &format!("# error: {e}\n"));
                }
            }
        });
        Ok(path)
    }

    /// `:diagnose --pprof[=N]` — drop the existing diagnostic text into
    /// a fresh directory, then asynchronously fetch
    /// `/debug/pprof/profile?seconds=N` and `/debug/pprof/trace?seconds=N`
    /// and write each as a sibling file. The operator's command-status
    /// row gets a "running" notice immediately; the final bundle path
    /// (or error) lands via `cmd_status_tx` when the pprof block ends.
    ///
    /// Pprof endpoints live on Bee's debug API. When operators
    /// haven't enabled `--debug-api-enable=true` the endpoint 404s;
    /// the helper translates that into a clear "enable Bee's debug
    /// API" hint.
    fn start_diagnose_with_pprof(&self, seconds: u32) -> CommandStatus {
        let secs_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!("bee-tui-diagnostic-{secs_unix}"));
        if let Err(e) = std::fs::create_dir_all(&dir) {
            return CommandStatus::Err(format!("diagnose --pprof: mkdir failed: {e}"));
        }
        let bundle_text = self.render_diagnostic_bundle();
        if let Err(e) = std::fs::write(dir.join("bundle.txt"), &bundle_text) {
            return CommandStatus::Err(format!("diagnose --pprof: write bundle.txt: {e}"));
        }
        // Resolve the active node's auth token so the pprof fetch
        // carries the same Authorization header bee-tui uses for the
        // regular API. Bee's debug-api-addr inherits the token when
        // it's served on the same listener.
        let auth_token = self
            .config
            .nodes
            .iter()
            .find(|n| n.name == self.api.name)
            .and_then(|n| n.resolved_token());
        let base_url = self.api.url.clone();
        let dir_for_task = dir.clone();
        let tx = self.cmd_status_tx.clone();
        tokio::spawn(async move {
            let r =
                pprof_bundle::fetch_and_write(&base_url, auth_token, seconds, dir_for_task).await;
            let status = match r {
                Ok(b) => CommandStatus::Info(b.summary()),
                Err(e) => CommandStatus::Err(format!("diagnose --pprof failed: {e}")),
            };
            let _ = tx.send(status);
        });
        CommandStatus::Info(format!(
            "diagnose --pprof={seconds}s in flight (bundle.txt already at {}; profile + trace will join when sampling completes)",
            dir.display()
        ))
    }

    fn export_diagnostic_bundle(&self) -> std::io::Result<PathBuf> {
        let bundle = self.render_diagnostic_bundle();
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!("bee-tui-diagnostic-{secs}.txt"));
        std::fs::write(&path, bundle)?;
        Ok(path)
    }

    fn render_diagnostic_bundle(&self) -> String {
        let now = format_utc_now();
        let health = self.health_rx.borrow().clone();
        let topology = self.watch.topology().borrow().clone();
        let stamps = self.watch.stamps().borrow().clone();
        let gates = Health::gates_for_with_stamps(&health, Some(&topology), Some(&stamps));
        let recent: Vec<_> = log_capture::handle()
            .map(|c| {
                let mut snap = c.snapshot();
                let len = snap.len();
                if len > 50 {
                    snap.drain(0..len - 50);
                }
                snap
            })
            .unwrap_or_default();

        let mut out = String::new();
        out.push_str("# bee-tui diagnostic bundle\n");
        out.push_str(&format!("# generated UTC {now}\n\n"));
        out.push_str("## profile\n");
        out.push_str(&format!("  name      {}\n", self.api.name));
        out.push_str(&format!("  endpoint  {}\n\n", self.api.url));
        out.push_str("## health gates\n");
        for g in &gates {
            out.push_str(&format_gate_line(g));
        }
        out.push_str("\n## last API calls (path only — Bearer tokens, if any, live in headers and aren't captured)\n");
        for e in &recent {
            let status = e
                .status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into());
            let elapsed = e
                .elapsed_ms
                .map(|ms| format!("{ms}ms"))
                .unwrap_or_else(|| "—".into());
            out.push_str(&format!(
                "  {ts} {method:<5} {path:<32} {status:>4} {elapsed:>7}\n",
                ts = e.ts,
                method = e.method,
                path = path_only(&e.url),
                status = status,
                elapsed = elapsed,
            ));
        }
        out.push_str(&format!(
            "\n## generated by bee-tui {}\n",
            env!("CARGO_PKG_VERSION"),
        ));
        out
    }

    /// Drive the batch-economics modal state machine on each key.
    /// Three phases:
    /// 1. **No action selected** — accept the `t/d/e/b/p` letter.
    /// 2. **Filling fields** — append printable chars, Backspace
    ///    deletes, Enter commits the field. After the last field
    ///    commits we compute the preview by re-using the existing
    ///    `run_*_preview` methods (whose verb-line parsers are the
    ///    single source of truth for arg validation).
    /// 3. **Result shown** — Enter / Esc dismiss.
    fn handle_batch_modal_key(&mut self, key: crossterm::event::KeyEvent) {
        use crossterm::event::KeyCode;
        if matches!(key.code, KeyCode::Esc) {
            self.batch_modal = BatchModal::default();
            return;
        }
        if self.batch_modal.result.is_some() {
            // Phase 3: any Enter dismisses. Other keys are no-ops so
            // an accidental keystroke doesn't trash the result.
            if matches!(key.code, KeyCode::Enter) {
                self.batch_modal = BatchModal::default();
            }
            return;
        }
        if self.batch_modal.action.is_none() {
            // Phase 1: a single letter picks the action.
            if let KeyCode::Char(c) = key.code {
                if let Some(a) = BatchAction::from_char(c) {
                    self.batch_modal.action = Some(a);
                }
            }
            return;
        }
        // Phase 2: filling fields.
        match key.code {
            KeyCode::Char(c) => {
                self.batch_modal.input_buffer.push(c);
            }
            KeyCode::Backspace => {
                self.batch_modal.input_buffer.pop();
            }
            KeyCode::Enter => {
                if self.batch_modal.input_buffer.trim().is_empty() {
                    return;
                }
                let committed = std::mem::take(&mut self.batch_modal.input_buffer);
                self.batch_modal.field_inputs.push(committed);
                // If we filled every field, compute the preview.
                let action = self.batch_modal.action.expect("phase guard");
                if self.batch_modal.field_inputs.len() >= action.fields().len() {
                    self.batch_modal.result = Some(self.compute_batch_modal_preview());
                }
            }
            _ => {}
        }
    }

    /// Reconstruct the verb-line from the modal inputs and dispatch
    /// to the existing `run_*_preview` method. Returns the preview
    /// text (or error message) ready to display in the modal's
    /// result panel.
    fn compute_batch_modal_preview(&self) -> String {
        let Some(action) = self.batch_modal.action else {
            return "internal error: no action selected".into();
        };
        let line = format!(
            "{} {}",
            action.verb(),
            self.batch_modal.field_inputs.join(" ")
        );
        let status = match action {
            BatchAction::Topup => self.run_topup_preview(&line),
            BatchAction::Dilute => self.run_dilute_preview(&line),
            BatchAction::Extend => self.run_extend_preview(&line),
            BatchAction::Buy => self.run_buy_preview(&line),
            BatchAction::Plan => self.run_plan_batch(&line),
        };
        match status {
            CommandStatus::Info(s) => s,
            CommandStatus::Err(s) => format!("error: {s}"),
        }
    }

    /// Per-Tick auto-restart watchdog for the supervised Bee child.
    /// No-op when `[bee.supervisor].auto_restart = false` or when
    /// bee-tui isn't acting as the supervisor at all. When enabled
    /// and the child has exited:
    /// 1. Check the sliding one-hour restart budget. If exhausted,
    ///    leave the supervisor dead and let the top-bar chip surface
    ///    "max restarts hit" — operator intervention required.
    /// 2. Check the exponential-backoff window. If we're still in
    ///    it, wait this tick.
    /// 3. Otherwise call `BeeSupervisor::spawn` (sync — just fork+exec)
    ///    to replace the dead child. The new child's `/health` will
    ///    come up on its own; we don't await it here because that
    ///    would block the tick. The next tick's `try_wait` flip
    ///    sets `bee_status` back to `Running` once Bee responds.
    fn tick_supervisor_watchdog(&mut self) {
        let Some(watchdog) = self.supervisor_watchdog.as_mut() else {
            return;
        };
        if self.bee_status.is_running() {
            return;
        }
        let now = Instant::now();
        if !watchdog.should_attempt(now) {
            return;
        }
        let bin = watchdog.bin.clone();
        let cfg = watchdog.config.clone();
        let logs = watchdog.logs.clone();
        watchdog.record_attempt(now);
        match BeeSupervisor::spawn(&bin, &cfg, logs) {
            Ok(sup) => {
                tracing::info!(
                    "bee-supervisor: restart #{} spawned (next-allowed after backoff)",
                    watchdog.restart_count
                );
                self.supervisor = Some(sup);
                self.bee_status = BeeStatus::Running;
            }
            Err(e) => {
                // Spawn-time failure (binary moved, FD exhaustion,
                // ...). Don't replace self.supervisor with None —
                // we want try_wait to keep reporting whatever the
                // last status was. Just record the attempt and let
                // the next tick try again after the backoff.
                tracing::warn!("bee-supervisor: restart attempt failed: {e}");
            }
        }
    }

    /// Per-Tick fleet-aggregate webhook. No-op when
    /// `[fleet].aggregate_webhook_url` is unset. When configured:
    /// 1. Ingest the current fleet snapshot, buffering any
    ///    worth-alerting status transitions.
    /// 2. If the coalesce window has elapsed, drain the buffer
    ///    and POST one consolidated message.
    ///
    /// Per-node `[alerts].webhook_url` keeps working independently
    /// — fleet aggregation sits *on top of* that, not in place of.
    fn tick_fleet_aggregate(&mut self) {
        let window = Duration::from_secs(self.config.fleet.aggregate_window_secs.max(1));
        let snapshot = self.fleet_rx.borrow().clone();
        let now = Instant::now();
        // Detect new transitions every tick. The notification
        // center consumes them immediately (one toast per node
        // transition); the webhook aggregator coalesces them
        // across the window for downstream pings.
        let new_transitions = self.fleet_aggregator.ingest_snapshot(&snapshot, now);
        if new_transitions > 0 {
            // Snapshot just the tail of `pending` we appended this
            // tick — older entries already fired into toasts on
            // earlier ticks.
            let pending = self.fleet_aggregator.pending.clone();
            let new_entries: Vec<&FleetAlertEntry> = pending
                .iter()
                .rev()
                .take(new_transitions)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            for entry in new_entries {
                let severity = severity_from_fleet_entry(entry);
                let headline = format!("fleet/{}: {:?} → {:?}", entry.node, entry.from, entry.to);
                self.notifications.ingest(
                    crate::notifications::Notification {
                        at: now,
                        severity,
                        headline,
                        why: entry.why.clone(),
                    },
                    &self.config.notifications,
                    now,
                );
            }
        }
        // Webhook escalation: only when [fleet].aggregate_webhook_url
        // is set. The window controls how often this fires; toasts
        // above fired per-transition already.
        let Some(url) = self
            .config
            .fleet
            .aggregate_webhook_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| u.to_string())
        else {
            // No webhook — still drain the window so the
            // aggregator doesn't accumulate stale entries forever.
            let _ = self.fleet_aggregator.drain_if_window_elapsed(now, window);
            return;
        };
        let Some(entries) = self.fleet_aggregator.drain_if_window_elapsed(now, window) else {
            return;
        };
        let message = FleetAggregator::format_message(&entries);
        tokio::spawn(async move {
            let client = match reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .user_agent(concat!("bee-tui/", env!("CARGO_PKG_VERSION")))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    tracing::warn!(target: "bee_tui::alerts", "fleet-aggregate client build failed: {e}");
                    return;
                }
            };
            // Slack/Discord-compatible payload: `{ "text": "..." }`.
            // Same shape used by the per-node alerter, so operators
            // can point both knobs at the same channel and get a
            // coherent format.
            let body = serde_json::json!({ "text": message });
            match client.post(&url).json(&body).send().await {
                Ok(resp) if resp.status().is_success() => {}
                Ok(resp) => tracing::warn!(
                    target: "bee_tui::alerts",
                    "fleet-aggregate webhook returned non-2xx: {}",
                    resp.status()
                ),
                Err(e) => tracing::warn!(
                    target: "bee_tui::alerts",
                    "fleet-aggregate webhook POST failed: {e}"
                ),
            }
        });
    }

    /// Per-Tick webhook alerter. No-op when `[alerts].webhook_url`
    /// is unset — operators get the cockpit's existing visual gates
    /// without any outbound traffic. When configured, we compute the
    /// same `Health::gates_for(...)` view the cockpit renders, diff
    /// against the previous Tick's status, and POST one webhook per
    /// transition that survives the per-gate debounce.
    fn tick_alerts(&mut self) {
        let health = self.health_rx.borrow().clone();
        let topology = self.watch.topology().borrow().clone();
        let stamps = self.watch.stamps().borrow().clone();
        let gates = Health::gates_for_with_stamps(&health, Some(&topology), Some(&stamps));
        let alerts = self.alert_state.diff_and_record(&gates);
        // Always feed alerts into the in-cockpit notification
        // center (v1.14). The webhook fire below is opt-in, but
        // toasts and the history overlay should reflect every
        // gate transition the cockpit detects regardless.
        let now = Instant::now();
        for alert in &alerts {
            let severity = severity_from_alert(alert);
            // `Unknown → X` here is a *first observation* of an
            // already-adverse gate (see `Alert::is_worth_notifying`),
            // not a live transition — phrase it as a startup snapshot
            // rather than the misleading "Unknown → Warn".
            let headline = if alert.from == GateStatus::Unknown {
                format!("{}: {:?} at startup", alert.gate, alert.to)
            } else {
                format!("{}: {:?} → {:?}", alert.gate, alert.from, alert.to)
            };
            let why = alert.why.clone();
            self.notifications.ingest(
                crate::notifications::Notification {
                    at: now,
                    severity,
                    headline,
                    why,
                },
                &self.config.notifications,
                now,
            );
        }
        // Webhook escalation: only when [alerts].webhook_url is set,
        // and only for real transitions — `is_worth_alerting` filters
        // out the initial-adverse "at startup" notifications above so
        // a cockpit restart doesn't re-spam the channel.
        if let Some(url) = self
            .config
            .alerts
            .webhook_url
            .as_deref()
            .filter(|u| !u.is_empty())
            .map(|u| u.to_string())
        {
            for alert in alerts.into_iter().filter(|a| a.is_worth_alerting()) {
                let url = url.clone();
                tokio::spawn(async move {
                    if let Err(e) = crate::alerts::fire(&url, &alert).await {
                        tracing::warn!(target: "bee_tui::alerts", "webhook fire failed: {e}");
                    }
                });
            }
        }
    }

    fn handle_actions(&mut self, tui: &mut Tui) -> color_eyre::Result<()> {
        while let Ok(action) = self.action_rx.try_recv() {
            if action != Action::Tick && action != Action::Render {
                debug!("{action:?}");
            }
            match action {
                Action::Tick => {
                    self.last_tick_key_events.drain(..);
                    // Advance the cold-start spinner once per tick
                    // so every screen's "loading…" line shows
                    // motion at a consistent cadence.
                    theme::advance_spinner();
                    // Refresh the supervised Bee's status (cheap
                    // non-blocking try_wait). Surfaced in the top
                    // bar so a mid-session crash is visible.
                    if let Some(sup) = self.supervisor.as_mut() {
                        self.bee_status = sup.status();
                    }
                    // Auto-restart watchdog. No-op when not
                    // configured. When configured and Bee isn't
                    // running, decides whether to respawn now or
                    // wait out the backoff window.
                    self.tick_supervisor_watchdog();
                    // Drain any newly-tailed Bee log lines into the
                    // log pane. Bounded loop — the channel is
                    // unbounded but try_recv stops at the first
                    // empty so we don't block the tick.
                    if let Some(rx) = self.bee_log_rx.as_mut() {
                        while let Ok((tab, line)) = rx.try_recv() {
                            self.log_pane.push_bee(tab, line);
                        }
                    }
                    // Surface async command-result updates (e.g.
                    // `:probe-upload` finished). The latest message
                    // wins — earlier ones get implicitly overwritten
                    // because we keep the loop draining.
                    while let Ok(status) = self.cmd_status_rx.try_recv() {
                        self.command_status = Some(status);
                    }
                    // Drain durability-check completions into the
                    // S13 Watchlist screen. Late results are still
                    // recorded — operators want to see every check
                    // they fired, not just the most recent.
                    while let Ok(result) = self.durability_rx.try_recv() {
                        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Watchlist") {
                            if let Some(wl) = self
                                .screens
                                .get_mut(idx)
                                .and_then(|s| s.as_any_mut())
                                .and_then(|a| a.downcast_mut::<Watchlist>())
                            {
                                wl.record(result);
                            }
                        }
                    }
                    // Drain feed-timeline walk completions into S14.
                    // Newest message wins — operator can fire a fresh
                    // :feed-timeline mid-walk and the in-flight result
                    // will overwrite this immediately.
                    while let Ok(msg) = self.feed_timeline_rx.try_recv() {
                        if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "FeedTimeline") {
                            if let Some(ft) = self
                                .screens
                                .get_mut(idx)
                                .and_then(|s| s.as_any_mut())
                                .and_then(|a| a.downcast_mut::<FeedTimeline>())
                            {
                                match msg {
                                    FeedTimelineMessage::Loaded(t) => ft.set_timeline(t),
                                    FeedTimelineMessage::Failed(e) => ft.set_error(e),
                                }
                            }
                        }
                    }
                    // Drain pubsub messages into S15 + sync the
                    // active-subs count so the header reflects
                    // start/stop verb activity even on a quiet topic.
                    let mut buffered: Vec<crate::pubsub::PubsubMessage> = Vec::new();
                    while let Ok(msg) = self.pubsub_msg_rx.try_recv() {
                        buffered.push(msg);
                    }
                    if let Some(idx) = SCREEN_NAMES.iter().position(|n| *n == "Pubsub") {
                        if let Some(ps) = self
                            .screens
                            .get_mut(idx)
                            .and_then(|s| s.as_any_mut())
                            .and_then(|a| a.downcast_mut::<Pubsub>())
                        {
                            for m in buffered {
                                ps.record(m);
                            }
                            ps.set_active_count(self.pubsub_subs.len());
                        }
                    }
                    // Webhook health-gate alerts. Cheap when not
                    // configured (no clones, no work) — only computes
                    // gates and diffs when [alerts].webhook_url is set.
                    self.tick_alerts();
                    // Fleet-aggregate webhook. Cheap when not configured.
                    self.tick_fleet_aggregate();
                    // Drop expired toasts so the overlay slot
                    // empties once auto-dismiss elapses.
                    self.notifications.purge_expired(Instant::now());
                }
                Action::Quit => self.should_quit = true,
                Action::Suspend => self.should_suspend = true,
                Action::Resume => self.should_suspend = false,
                Action::ClearScreen => tui.terminal.clear()?,
                Action::Resize(w, h) => self.handle_resize(tui, w, h)?,
                Action::Render => self.render(tui)?,
                Action::SwitchContext(ref target) => {
                    // Triggered by S15 Fleet's Enter binding. Same
                    // flow as the `:context` verb / Ctrl-N picker.
                    self.command_status = Some(match self.switch_context(target) {
                        Ok(()) => CommandStatus::Info(format!(
                            "switched to context {target} ({})",
                            self.api.url
                        )),
                        Err(e) => CommandStatus::Err(format!("context switch failed: {e}")),
                    });
                }
                _ => {}
            }
            let tx = self.action_tx.clone();
            for component in self.iter_components_mut() {
                if let Some(action) = component.update(action.clone())? {
                    tx.send(action)?
                };
            }
        }
        Ok(())
    }

    fn handle_resize(&mut self, tui: &mut Tui, w: u16, h: u16) -> color_eyre::Result<()> {
        tui.resize(Rect::new(0, 0, w, h))?;
        self.render(tui)?;
        Ok(())
    }

    fn render(&mut self, tui: &mut Tui) -> color_eyre::Result<()> {
        let active = self.current_screen;
        let tx = self.action_tx.clone();
        let screens = &mut self.screens;
        let log_pane = &mut self.log_pane;
        let log_pane_height = log_pane.height();
        let command_buffer = self.command_buffer.clone();
        let command_suggestion_index = self.command_suggestion_index;
        let command_status = self.command_status.clone();
        let help_visible = self.help_visible;
        let help_page = self.help_page;
        let profile = self.api.name.clone();
        let endpoint = self.api.url.clone();
        let last_ping = self.health_rx.borrow().last_ping;
        let now_utc = format_utc_now();
        let bee_status_label = if self.supervisor.is_some() {
            let running = self.bee_status.is_running();
            // With the watchdog active we always surface the chip so
            // operators see uptime + restart count at a glance. Without
            // a watchdog we keep the v1.11 behaviour: only show the
            // chip when something is wrong, so a healthy cockpit's
            // metadata line stays calm.
            match self.supervisor_watchdog.as_ref() {
                Some(w) => {
                    let uptime = self.supervisor.as_ref().map(|s| s.uptime());
                    Some((w.top_bar_label(running, uptime), running))
                }
                None if !running => Some((self.bee_status.label(), false)),
                None => None,
            }
        } else {
            None
        };
        // Background-task awareness chips. Hidden when there's nothing
        // to surface — keeps the top bar quiet on a fresh session and
        // visibly busy when daemons are running.
        let pubsub_subs_count = self.pubsub_subs.len();
        let watch_refs_count = self.watch_refs.len();
        let alerts_enabled = self
            .config
            .alerts
            .webhook_url
            .as_deref()
            .is_some_and(|u| !u.is_empty());
        // Unread-notification count — the persistent, glanceable cue
        // a toast can't be (toasts auto-dismiss). Cleared to 0 when
        // the operator opens the `Ctrl+Alt+N` history overlay.
        let unread_notifs = self.notifications.unread_count();
        // Node picker overlay state — clamp the cursor every render
        // so a config reload that shrunk `nodes` can't leave it
        // pointing past the end.
        let nodes_picker_visible = self.nodes_picker_visible;
        if !self.config.nodes.is_empty() && self.nodes_picker_selected >= self.config.nodes.len() {
            self.nodes_picker_selected = self.config.nodes.len() - 1;
        }
        let nodes_picker_selected = self.nodes_picker_selected;
        let nodes_picker_rows: Vec<(String, String, bool, bool)> = self
            .config
            .nodes
            .iter()
            .map(|n| {
                (
                    n.name.clone(),
                    n.url.clone(),
                    n.default,
                    n.name == self.api.name,
                )
            })
            .collect();
        let batch_modal_visible = self.batch_modal.visible;
        let batch_modal_state = self.batch_modal.clone();
        let log_fullscreen = self.log_fullscreen;
        let visible_toasts = self.notifications.visible_toasts();
        let notifications_overlay_visible = self.notifications_overlay_visible;
        let notifications_history = if notifications_overlay_visible {
            self.notifications.history_newest_first()
        } else {
            Vec::new()
        };
        tui.draw(|frame| {
            use ratatui::layout::{Constraint, Layout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span};
            use ratatui::widgets::Paragraph;

            // Layout: in normal mode the active screen takes the
            // middle of the cockpit and the log pane sits below at
            // its configured height. When `Shift+L` flips
            // `log_fullscreen`, we swap the roles: the screen
            // collapses to 0 lines and the log pane absorbs the
            // middle. Top bar + command bar stay put either way so
            // the operator never loses sight of context or input.
            let chunks = if log_fullscreen {
                Layout::vertical([
                    Constraint::Length(2), // top-bar (metadata + tabs)
                    Constraint::Length(0), // (hidden screen body)
                    Constraint::Length(1), // command bar / status line
                    Constraint::Min(0),    // log pane fills the rest
                ])
                .split(frame.area())
            } else {
                Layout::vertical([
                    Constraint::Length(2),               // top-bar (metadata + tabs)
                    Constraint::Min(0),                  // active screen
                    Constraint::Length(1),               // command bar / status line
                    Constraint::Length(log_pane_height), // tabbed log pane
                ])
                .split(frame.area())
            };

            let top_chunks =
                Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).split(chunks[0]);

            // Metadata line: profile · endpoint · ping · clock.
            let ping_str = match last_ping {
                Some(d) => format!("{}ms", d.as_millis()),
                None => "—".into(),
            };
            let t = theme::active();
            let mut metadata_spans = vec![
                Span::styled(
                    " bee-tui ",
                    Style::default()
                        .fg(Color::Black)
                        .bg(t.info)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    profile,
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" @ {endpoint}"), Style::default().fg(t.dim)),
                Span::raw("   "),
                Span::styled("ping ", Style::default().fg(t.dim)),
                Span::styled(ping_str, Style::default().fg(t.info)),
                Span::raw("   "),
                Span::styled(format!("UTC {now_utc}"), Style::default().fg(t.dim)),
            ];
            // Append a Bee-process status chip iff the supervisor is
            // active AND something is wrong. Renders red so a crash
            // mid-session is impossible to miss in the top bar.
            if let Some((label, running)) = bee_status_label.as_ref() {
                metadata_spans.push(Span::raw("   "));
                let bg = if *running { t.pass } else { t.fail };
                metadata_spans.push(Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(bg)
                        .add_modifier(Modifier::BOLD),
                ));
            }
            // Background-task chips: pubsub subscriptions, watch-ref
            // daemons, and alerts state. Each is hidden when there's
            // nothing to surface so the line stays calm on a fresh
            // session. Operator notices stuff is running when the
            // chip appears, not by remembering to type a verb.
            if pubsub_subs_count > 0 {
                metadata_spans.push(Span::raw("   "));
                metadata_spans.push(Span::styled("subs ", Style::default().fg(t.dim)));
                metadata_spans.push(Span::styled(
                    format!("{pubsub_subs_count}"),
                    Style::default().fg(t.info).add_modifier(Modifier::BOLD),
                ));
            }
            if watch_refs_count > 0 {
                metadata_spans.push(Span::raw("   "));
                metadata_spans.push(Span::styled("watch ", Style::default().fg(t.dim)));
                metadata_spans.push(Span::styled(
                    format!("{watch_refs_count}"),
                    Style::default().fg(t.info).add_modifier(Modifier::BOLD),
                ));
            }
            if alerts_enabled {
                metadata_spans.push(Span::raw("   "));
                metadata_spans.push(Span::styled("alerts ", Style::default().fg(t.dim)));
                metadata_spans.push(Span::styled(
                    "●",
                    Style::default().fg(t.pass).add_modifier(Modifier::BOLD),
                ));
            }
            // Unread-notification chip. Hidden at zero; coloured warn
            // so a new alert is noticeable at a glance even if the
            // operator missed the toast. `Ctrl+Alt+N` clears it.
            if unread_notifs > 0 {
                metadata_spans.push(Span::raw("   "));
                metadata_spans.push(Span::styled("notif ", Style::default().fg(t.dim)));
                metadata_spans.push(Span::styled(
                    format!("{unread_notifs}"),
                    Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
                ));
            }
            let metadata_line = Line::from(metadata_spans);
            frame.render_widget(Paragraph::new(metadata_line), top_chunks[0]);

            // Tab strip with the active screen highlighted.
            let theme = *theme::active();
            let mut tabs = Vec::with_capacity(SCREEN_NAMES.len() * 2);
            for (i, name) in SCREEN_NAMES.iter().enumerate() {
                let style = if i == active {
                    Style::default()
                        .fg(theme.tab_active_fg)
                        .bg(theme.tab_active_bg)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(theme.dim)
                };
                tabs.push(Span::styled(format!(" {name} "), style));
                tabs.push(Span::raw(" "));
            }
            tabs.push(Span::styled(
                ":cmd · Tab to cycle · ? help",
                Style::default().fg(theme.dim),
            ));
            frame.render_widget(Paragraph::new(Line::from(tabs)), top_chunks[1]);

            // Active screen — skipped when the log pane has been
            // expanded to fullscreen via `Shift+L` (the chunks[1]
            // rect is Length(0) in that mode and screens that
            // allocate via Layout::vertical would otherwise spam
            // ratatui with zero-size rect warnings).
            if !log_fullscreen {
                if let Some(screen) = screens.get_mut(active) {
                    if let Err(err) = screen.draw(frame, chunks[1]) {
                        let _ = tx.send(Action::Error(format!("Failed to draw screen: {err:?}")));
                    }
                }
            }
            // Command bar / status line
            let prompt = if let Some(buf) = &command_buffer {
                Line::from(vec![
                    Span::styled(
                        ":",
                        Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(buf.clone(), Style::default().add_modifier(Modifier::BOLD)),
                    Span::styled("█", Style::default().fg(t.accent)),
                ])
            } else {
                match &command_status {
                    Some(CommandStatus::Info(msg)) => {
                        Line::from(Span::styled(msg.clone(), Style::default().fg(t.pass)))
                    }
                    Some(CommandStatus::Err(msg)) => {
                        Line::from(Span::styled(msg.clone(), Style::default().fg(t.fail)))
                    }
                    None => Line::from(""),
                }
            };
            frame.render_widget(Paragraph::new(prompt), chunks[2]);

            // Command suggestion popup — floats above the command bar
            // while the operator is typing. Filtered list of known
            // verbs that prefix-match the buffer's first token; Up/Down
            // navigates, Tab completes. Skipped silently if the
            // command bar is closed or no commands match.
            if let Some(buf) = &command_buffer {
                let matches = filter_command_suggestions(buf, KNOWN_COMMANDS);
                if !matches.is_empty() {
                    draw_command_suggestions(
                        frame,
                        chunks[2],
                        &matches,
                        command_suggestion_index,
                        &theme,
                    );
                }
            }

            // Tabbed log pane
            if let Err(err) = log_pane.draw(frame, chunks[3]) {
                let _ = tx.send(Action::Error(format!("Failed to draw log: {err:?}")));
            }

            // Help overlay — drawn last so it floats above everything
            // else. Centred with a fixed width that fits even narrow
            // terminals (≥60 cols). Falls back to the full screen on
            // anything narrower.
            if help_visible {
                draw_help_overlay(frame, frame.area(), active, help_page, &theme);
            }
            if nodes_picker_visible {
                draw_nodes_picker(
                    frame,
                    frame.area(),
                    &nodes_picker_rows,
                    nodes_picker_selected,
                    &theme,
                );
            }
            if batch_modal_visible {
                draw_batch_modal(frame, frame.area(), &batch_modal_state, &theme);
            }
            // Top-right toast stack — drawn after every modal so
            // alerts still register even with help / picker open.
            // Skipped when the operator has the history overlay
            // open (they're already looking at notifications).
            if !visible_toasts.is_empty() && !notifications_overlay_visible {
                draw_toasts(frame, frame.area(), &visible_toasts, &theme);
            }
            if notifications_overlay_visible {
                draw_notifications_overlay(frame, frame.area(), &notifications_history, &theme);
            }
        })?;
        Ok(())
    }
}

/// Render the command-suggestion popup just above the command bar.
/// Floats over the active screen (uses `Clear` to blank what's
/// underneath) and highlights the row at `selected` so Up/Down
/// navigation is visible. Auto-scrolls if the filtered list exceeds
/// the visible window — operators see at most `MAX_VISIBLE` rows at
/// a time.
fn draw_command_suggestions(
    frame: &mut ratatui::Frame,
    bar_rect: ratatui::layout::Rect,
    matches: &[&(&str, &str)],
    selected: usize,
    theme: &theme::Theme,
) {
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    const MAX_VISIBLE: usize = 10;
    let visible_rows = matches.len().min(MAX_VISIBLE);
    if visible_rows == 0 {
        return;
    }
    let height = (visible_rows as u16) + 2; // +2 for top + bottom borders
    // Width = longest "name  description" line + borders + padding,
    // capped at 80% of the screen so wide descriptions don't push
    // the popup off the edge.
    let widest = matches
        .iter()
        .map(|(name, desc)| name.len() + desc.len() + 6)
        .max()
        .unwrap_or(40)
        .min(bar_rect.width as usize);
    let width = (widest as u16 + 2).min(bar_rect.width);
    // Anchor above the command bar; if the popup would clip the top
    // of the screen, fall back to as much vertical room as we have.
    let bottom = bar_rect.y;
    let y = bottom.saturating_sub(height);
    let popup = Rect {
        x: bar_rect.x,
        y,
        width,
        height: bottom - y,
    };

    // Auto-scroll: keep `selected` inside the visible window.
    let scroll_start = if selected >= visible_rows {
        selected + 1 - visible_rows
    } else {
        0
    };
    let visible_slice = &matches[scroll_start..(scroll_start + visible_rows).min(matches.len())];

    let mut lines: Vec<Line> = Vec::with_capacity(visible_slice.len());
    for (i, (name, desc)) in visible_slice.iter().enumerate() {
        let absolute_idx = scroll_start + i;
        let is_selected = absolute_idx == selected;
        let row_style = if is_selected {
            Style::default()
                .fg(theme.tab_active_fg)
                .bg(theme.tab_active_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        let cursor = if is_selected { "▸ " } else { "  " };
        lines.push(Line::from(vec![
            Span::styled(format!("{cursor}:{name:<16}  "), row_style),
            Span::styled(
                desc.to_string(),
                if is_selected {
                    row_style
                } else {
                    Style::default().fg(theme.dim)
                },
            ),
        ]));
    }

    // Title shows pagination state when the list overflows.
    let title = if matches.len() > MAX_VISIBLE {
        format!(" :commands ({}/{}) ", selected + 1, matches.len())
    } else {
        " :commands ".to_string()
    };

    frame.render_widget(Clear, popup);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(title),
        ),
        popup,
    );
}

/// Render the `?` help overlay. Pulls a per-screen keymap from
/// [`screen_keymap`] and pairs it with the global keys (Tab, `:`,
/// `q`). Drawn as a centred floating box; everything outside is
/// dimmed via a [`Clear`] underlay.
fn draw_help_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    active_screen: usize,
    page: HelpPage,
    theme: &theme::Theme,
) {
    use ratatui::layout::Rect;
    use ratatui::style::Style;
    use ratatui::text::Line;
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    // Overlay box: bigger when we need the verb catalogue to fit
    // (it has 50+ rows including headings).
    let (w_max, h_max) = match page {
        HelpPage::Keys => (72, 22),
        HelpPage::Verbs => (84, 40),
    };
    let w = area.width.min(w_max);
    let h = area.height.min(h_max);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let lines: Vec<Line> = match page {
        HelpPage::Keys => build_help_keys_lines(active_screen, theme),
        HelpPage::Verbs => build_help_verbs_lines(theme),
    };

    let title = match page {
        HelpPage::Keys => " help — keys ",
        HelpPage::Verbs => " help — verbs ",
    };
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(title),
        ),
        rect,
    );
}

fn build_help_keys_lines<'a>(
    active_screen: usize,
    theme: &theme::Theme,
) -> Vec<ratatui::text::Line<'a>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    let screen_name = SCREEN_NAMES.get(active_screen).copied().unwrap_or("?");
    let screen_rows = screen_keymap(active_screen);
    let global_rows: &[(&str, &str)] = &[
        ("Tab / Shift+Tab", "cycle screen"),
        ("1-9 / 0", "jump to S1-S9 / S10"),
        ("Alt+1..Alt+5", "jump to S11-S15"),
        ("Ctrl+N", "open node picker (also :nodes)"),
        (
            "Ctrl+Alt+N",
            "open notification history overlay (also :notifications)",
        ),
        (
            "E",
            "open batch-economics modal (topup/dilute/extend/buy/plan)",
        ),
        (
            "Shift+L",
            "toggle fullscreen log pane (collapses active screen)",
        ),
        ("/", "filter the log pane (case-insensitive substring)"),
        ("[ / ]", "previous / next log-pane tab"),
        ("+ / -", "grow / shrink log pane"),
        ("Shift+↑/↓", "scroll log pane (1 line); pauses auto-tail"),
        ("Shift+PgUp/PgDn", "scroll log pane (10 lines)"),
        ("Shift+←/→", "pan log pane horizontally (8 cols)"),
        ("Shift+End", "resume auto-tail + reset horizontal pan"),
        ("?", "toggle this help"),
        (":", "open command bar"),
        ("qq", "quit (double-tap; or :q)"),
        ("Ctrl+C / Ctrl+D", "quit immediately"),
    ];

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {screen_name} "),
            Style::default()
                .fg(theme.tab_active_fg)
                .bg(theme.tab_active_bg)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("   screen-specific keys"),
    ]));
    lines.push(Line::from(""));
    if screen_rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no extra keys for this screen — use the command bar via :)",
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        for (key, desc) in screen_rows {
            lines.push(format_help_row(key, desc, theme));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  global",
        Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
    )));
    for (key, desc) in global_rows {
        lines.push(format_help_row(key, desc, theme));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Tab to verb list   Esc / ? / q to dismiss",
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    lines
}

/// Categorise every `KNOWN_COMMANDS` entry. Each verb maps to one
/// category; the catalogue ordering inside a category preserves the
/// `KNOWN_COMMANDS` order so the popup and the verb list agree.
fn verb_category(name: &str) -> &'static str {
    match name {
        "health" | "stamps" | "swap" | "lottery" | "peers" | "network" | "warmup" | "api"
        | "tags" | "pins" | "watchlist" | "fleet" => "navigate",
        "topup-preview" | "dilute-preview" | "extend-preview" | "buy-preview" | "buy-suggest"
        | "plan-batch" | "price" | "basefee" => "stamps & economics",
        "probe-upload" | "upload-file" | "upload-collection" => "uploads",
        "feed-probe" | "feed-timeline" | "manifest" | "inspect" | "hash" | "cid"
        | "depth-table" | "grantees-list" => "inspect",
        "durability-check" | "watch-ref" | "watch-ref-stop" | "pins-check" => "durability",
        "pubsub-pss"
        | "pubsub-gsoc"
        | "pubsub-stop"
        | "pubsub-filter"
        | "pubsub-filter-clear"
        | "pubsub-replay" => "pubsub",
        "gsoc-mine" | "pss-target" => "mining / addresses",
        "check-version" | "config-doctor" | "diagnose" | "loggers" | "set-logger" => {
            "diagnostics & config"
        }
        "context" | "nodes" | "notifications" | "quit" => "cockpit",
        _ => "other",
    }
}

fn build_help_verbs_lines(theme: &theme::Theme) -> Vec<ratatui::text::Line<'static>> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};

    // Category order — matches the cockpit's typical reading flow.
    let order = [
        "navigate",
        "inspect",
        "stamps & economics",
        "uploads",
        "durability",
        "pubsub",
        "mining / addresses",
        "diagnostics & config",
        "cockpit",
    ];
    let mut lines: Vec<Line<'static>> =
        Vec::with_capacity(KNOWN_COMMANDS.len() + order.len() * 2 + 4);
    lines.push(Line::from(Span::styled(
        format!("  every :verb ({})", KNOWN_COMMANDS.len()),
        Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    for cat in order {
        let mut first = true;
        for (name, desc) in KNOWN_COMMANDS {
            if verb_category(name) != cat {
                continue;
            }
            if first {
                lines.push(Line::from(Span::styled(
                    format!("  {cat}"),
                    Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
                )));
                first = false;
            }
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<18}", format!(":{name}")),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(*desc),
            ]));
        }
        if !first {
            lines.push(Line::from(""));
        }
    }
    lines.push(Line::from(Span::styled(
        "  Tab to keys   Esc / ? / q to dismiss",
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));
    lines
}

/// Render the node-picker overlay. Rows are `(name, url, is_default,
/// is_active)`; `selected` is the cursor row. Floats centred and
/// auto-sizes to fit the configured `[[nodes]]` count plus a header
/// and footer. Used by both Ctrl-N and the `:nodes` verb.
fn draw_nodes_picker(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    rows: &[(String, String, bool, bool)],
    selected: usize,
    theme: &theme::Theme,
) {
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    // Auto-width: longest "name @ url" plus padding, clamped to 78.
    let name_w = rows.iter().map(|r| r.0.len()).max().unwrap_or(8);
    let url_w = rows.iter().map(|r| r.1.len()).max().unwrap_or(20);
    let needed = (name_w + url_w + 12) as u16;
    let w = area.width.min(needed.max(48)).min(80);
    let h = ((rows.len() + 4) as u16).clamp(6, area.height.min(20));
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::with_capacity(rows.len() + 2);
    if rows.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (no nodes configured — add [[nodes]] entries to config.toml)",
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        for (i, (name, url, is_default, is_active)) in rows.iter().enumerate() {
            let is_sel = i == selected;
            let cursor = if is_sel { "▸ " } else { "  " };
            let row_style = if is_sel {
                Style::default()
                    .fg(theme.tab_active_fg)
                    .bg(theme.tab_active_bg)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            // Active / default markers, right-aligned-ish for the eye.
            let mut tags = String::new();
            if *is_active {
                tags.push_str(" ●");
            }
            if *is_default {
                tags.push_str(" ★");
            }
            lines.push(Line::from(vec![
                Span::styled(format!("{cursor}{name:<name_w$}  "), row_style),
                Span::styled(
                    url.to_string(),
                    if is_sel {
                        row_style
                    } else {
                        Style::default().fg(theme.dim)
                    },
                ),
                Span::styled(
                    tags,
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  ↑/↓ select   Enter switch   Esc / Ctrl-N close   ● active  ★ default",
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(" nodes "),
        ),
        rect,
    );
}

/// Render the batch-economics modal. Walks the operator through
/// action choice → field entry → preview output, then dismisses on
/// Enter / Esc. Same overlay-with-Clear treatment as the help and
/// nodes-picker overlays.
fn draw_batch_modal(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    state: &BatchModal,
    theme: &theme::Theme,
) {
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let w = area.width.min(72);
    let h = area.height.min(16);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::new();

    match state.action {
        None => {
            lines.push(Line::from(Span::styled(
                "  Pick an action:",
                Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from(""));
            for (key, label, desc) in [
                (
                    "t",
                    "topup-preview",
                    "predict TTL gain from topping up an existing batch",
                ),
                (
                    "d",
                    "dilute-preview",
                    "predict utilisation drop from a higher depth",
                ),
                (
                    "e",
                    "extend-preview",
                    "what would N more seconds of TTL cost?",
                ),
                (
                    "b",
                    "buy-preview",
                    "predict TTL of a fresh batch at (depth, amount)",
                ),
                ("p", "plan-batch", "unified topup + dilute recommendation"),
            ] {
                lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("[{key}]"),
                        Style::default()
                            .fg(theme.accent)
                            .add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(" "),
                    Span::styled(
                        format!("{label:<16}"),
                        Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
                    ),
                    Span::raw("  "),
                    Span::styled(desc.to_string(), Style::default().fg(theme.dim)),
                ]));
            }
            lines.push(Line::from(""));
            lines.push(Line::from(Span::styled(
                "  Press the letter to choose · Esc cancels",
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        Some(action) => {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!(":{} ", action.verb()),
                    Style::default()
                        .fg(theme.accent)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "(Esc to cancel)",
                    Style::default()
                        .fg(theme.dim)
                        .add_modifier(Modifier::ITALIC),
                ),
            ]));
            lines.push(Line::from(""));
            // Committed fields
            let fields = action.fields();
            for (i, label) in fields.iter().enumerate() {
                if let Some(value) = state.field_inputs.get(i) {
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(format!("{label:<24}"), Style::default().fg(theme.dim)),
                        Span::raw(" "),
                        Span::styled(
                            value.clone(),
                            Style::default().fg(theme.info).add_modifier(Modifier::BOLD),
                        ),
                    ]));
                } else if i == state.field_inputs.len() && state.result.is_none() {
                    // Active prompt
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("{label:<24}"),
                            Style::default()
                                .fg(theme.accent)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Span::raw(" "),
                        Span::styled(
                            format!("> {}_", state.input_buffer),
                            Style::default().fg(theme.info),
                        ),
                    ]));
                } else {
                    // Future field, dim placeholder
                    lines.push(Line::from(vec![
                        Span::raw("  "),
                        Span::styled(
                            format!("{label:<24}"),
                            Style::default()
                                .fg(theme.dim)
                                .add_modifier(Modifier::ITALIC),
                        ),
                    ]));
                }
            }
            if let Some(result) = &state.result {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Result:",
                    Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
                )));
                for line in result.lines() {
                    lines.push(Line::from(vec![
                        Span::raw("    "),
                        Span::styled(line.to_string(), Style::default().fg(theme.info)),
                    ]));
                }
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Enter / Esc to dismiss",
                    Style::default()
                        .fg(theme.dim)
                        .add_modifier(Modifier::ITALIC),
                )));
            } else {
                lines.push(Line::from(""));
                lines.push(Line::from(Span::styled(
                    "  Type the value · Enter commits the field · Esc cancels",
                    Style::default()
                        .fg(theme.dim)
                        .add_modifier(Modifier::ITALIC),
                )));
            }
        }
    }

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(" batch economics "),
        ),
        rect,
    );
}

/// Render the top-right toast stack. Up to
/// `crate::notifications::MAX_VISIBLE_TOASTS` cards, each one ~3
/// rows tall (border + headline + dim why-line). The stack is
/// anchored 1 column / 1 row from the screen's top-right corner
/// so it never collides with the metadata line or the tab strip
/// at the top. Skips render when there's nothing to show.
fn draw_toasts(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    toasts: &[crate::notifications::Notification],
    theme: &theme::Theme,
) {
    use crate::notifications::NotificationSeverity;
    use ratatui::layout::Rect;
    use ratatui::style::{Color, Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    if toasts.is_empty() {
        return;
    }
    let toast_w = 48u16.min(area.width.saturating_sub(2));
    if toast_w < 20 {
        return; // Terminal too narrow for a useful toast.
    }
    let mut y_cursor = area.y.saturating_add(2); // below metadata + tabs
    for n in toasts {
        let why_present = n.why.is_some();
        let h: u16 = if why_present { 4 } else { 3 };
        if y_cursor.saturating_add(h) > area.y + area.height {
            break; // Out of vertical room.
        }
        let x = area
            .x
            .saturating_add(area.width)
            .saturating_sub(toast_w + 1);
        let rect = Rect {
            x,
            y: y_cursor,
            width: toast_w,
            height: h,
        };
        let (sev_fg, sev_bg) = match n.severity {
            NotificationSeverity::Fail => (Color::Black, theme.fail),
            NotificationSeverity::Warn => (Color::Black, theme.warn),
            NotificationSeverity::Recovery => (Color::Black, theme.pass),
            NotificationSeverity::Info => (Color::Black, theme.info),
        };
        let mut body_lines: Vec<Line> = Vec::with_capacity(2);
        body_lines.push(Line::from(vec![
            Span::styled(
                format!(" {} ", n.severity.label()),
                Style::default()
                    .fg(sev_fg)
                    .bg(sev_bg)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
            Span::styled(
                truncate_for_toast(&n.headline, (toast_w as usize).saturating_sub(10)),
                Style::default().add_modifier(Modifier::BOLD),
            ),
        ]));
        if let Some(why) = &n.why {
            body_lines.push(Line::from(Span::styled(
                truncate_for_toast(why, (toast_w as usize).saturating_sub(4)),
                Style::default()
                    .fg(theme.dim)
                    .add_modifier(Modifier::ITALIC),
            )));
        }
        let border_style = Style::default().fg(sev_bg);
        frame.render_widget(Clear, rect);
        frame.render_widget(
            Paragraph::new(body_lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(border_style),
            ),
            rect,
        );
        y_cursor = y_cursor.saturating_add(h);
    }
}

fn truncate_for_toast(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max.saturating_sub(1)).collect();
        out.push('…');
        out
    }
}

/// Render the centered notification-history overlay. Newest first
/// (operator wants reverse chronological scan).
fn draw_notifications_overlay(
    frame: &mut ratatui::Frame,
    area: ratatui::layout::Rect,
    history: &[crate::notifications::Notification],
    theme: &theme::Theme,
) {
    use crate::notifications::NotificationSeverity;
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let w = area.width.min(80);
    let h = area.height.min(28);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

    let mut lines: Vec<Line> = Vec::with_capacity(history.len() + 4);
    lines.push(Line::from(Span::styled(
        format!(
            "  {} notifications this session (newest first)",
            history.len()
        ),
        Style::default().fg(theme.dim).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));
    if history.is_empty() {
        lines.push(Line::from(Span::styled(
            "  (nothing yet — notifications fire on health-gate problems)",
            Style::default()
                .fg(theme.dim)
                .add_modifier(Modifier::ITALIC),
        )));
    } else {
        let now = std::time::Instant::now();
        for n in history {
            let sev_fg = match n.severity {
                NotificationSeverity::Fail => theme.fail,
                NotificationSeverity::Warn => theme.warn,
                NotificationSeverity::Recovery => theme.pass,
                NotificationSeverity::Info => theme.info,
            };
            let age = format_age(now.duration_since(n.at));
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    format!("{:<5}", n.severity.label()),
                    Style::default().fg(sev_fg).add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!(" {age:>6}  "), Style::default().fg(theme.dim)),
                Span::styled(
                    n.headline.clone(),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]));
            if let Some(why) = &n.why {
                lines.push(Line::from(vec![
                    Span::raw("              "),
                    Span::styled(
                        why.clone(),
                        Style::default()
                            .fg(theme.dim)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Esc to dismiss",
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));

    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(" notifications "),
        ),
        rect,
    );
}

fn format_age(d: std::time::Duration) -> String {
    let secs = d.as_secs();
    if secs < 60 {
        format!("{secs}s")
    } else if secs < 3600 {
        format!("{}m", secs / 60)
    } else if secs < 86_400 {
        format!("{}h", secs / 3600)
    } else {
        format!("{}d", secs / 86_400)
    }
}

fn format_help_row<'a>(
    key: &'a str,
    desc: &'a str,
    theme: &theme::Theme,
) -> ratatui::text::Line<'a> {
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    Line::from(vec![
        Span::raw("  "),
        Span::styled(
            format!("{key:<16}"),
            Style::default()
                .fg(theme.accent)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::raw(desc),
    ])
}

/// Per-screen keymap rows, indexed by the same position as
/// [`SCREEN_NAMES`]. Edit here when a screen grows new keys —
/// no other place needs updating.
fn screen_keymap(active_screen: usize) -> &'static [(&'static str, &'static str)] {
    match active_screen {
        // 0: Health — read-only
        1 => &[
            ("↑↓ / j k", "move row selection"),
            ("Enter", "drill batch — bucket histogram + worst-N"),
            ("Esc", "close drill"),
        ],
        // 2: Swap — read-only
        3 => &[("r", "run on-demand rchash benchmark")],
        4 => &[
            ("↑↓ / j k", "move peer selection"),
            (
                "Enter",
                "drill peer — balance / cheques / settlement / ping",
            ),
            ("Esc", "close drill"),
        ],
        // 5: Network — read-only
        // 6: Warmup — read-only
        // 7: API — read-only
        8 => &[
            ("↑↓ / j k", "scroll one row"),
            ("PgUp / PgDn", "scroll ten rows"),
            ("Home", "back to top"),
        ],
        // 9: Pins — selectable rows + on-demand integrity check.
        9 => &[
            ("↑↓ / j k", "move row selection"),
            ("Enter", "integrity-check the highlighted pin"),
            ("c", "integrity-check every unchecked pin"),
            ("s", "cycle sort: ref order / bad first / by size"),
        ],
        // 10: Manifest — Mantaray tree browser.
        10 => &[
            ("↑↓ / j k", "move row selection"),
            ("Enter", "expand / collapse fork (loads child chunk)"),
            (":manifest <ref>", "open a manifest at a reference"),
            (":inspect <ref>", "what is this? auto-detects manifest"),
        ],
        // 11: Watchlist — durability-check history.
        11 => &[
            ("↑↓ / j k", "move row selection"),
            (":durability-check <ref>", "walk chunk graph + record"),
        ],
        // 12: Feed Timeline — feed history walker.
        12 => &[
            ("↑↓ / j k", "move row selection"),
            ("PgUp / PgDn", "jump 10 rows"),
            (
                ":feed-timeline <owner> <topic> [N]",
                "load history (default 50)",
            ),
        ],
        // 13: Pubsub watch — live PSS / GSOC tail.
        13 => &[
            ("↑↓ / j k", "move row selection"),
            ("PgUp / PgDn", "jump 10 rows"),
            ("c", "clear timeline"),
            (":pubsub-pss <topic>", "subscribe to a PSS topic"),
            (":pubsub-gsoc <owner> <id>", "subscribe to a GSOC SOC"),
            (":pubsub-stop [sub-id]", "stop one (or all) subscriptions"),
            (
                ":pubsub-filter <substr>",
                "show only rows containing substring",
            ),
            (":pubsub-filter-clear", "remove the active filter"),
        ],
        // 14: Fleet — multi-node health roll-up.
        14 => &[
            ("↑↓ / j k", "move row selection"),
            ("Enter", "switch context to the cursored node"),
            ("r", "re-poll the fleet right now"),
        ],
        _ => &[],
    }
}

/// Construct every cockpit screen with receivers from the supplied
/// hub. Extracted so `App::new` and the `:context` profile-switcher
/// can share the wiring — the screen list is the same on every
/// connection, only the underlying watch hub changes.
///
/// Order matters — the [`SCREEN_NAMES`] table assumes index 0 is
/// Health, 1 is Stamps, 2 is Swap, 3 is Lottery, 4 is Peers, 5 is
/// Network, 6 is Warmup, 7 is API, 8 is Tags, 9 is Pins.
fn build_screens(
    api: &Arc<ApiClient>,
    watch: &BeeWatch,
    market_rx: Option<watch::Receiver<crate::economics_oracle::EconomicsSnapshot>>,
    fleet_rx: watch::Receiver<crate::fleet::FleetSnapshot>,
    fleet_resync_tx: tokio::sync::mpsc::UnboundedSender<()>,
) -> Vec<Box<dyn Component>> {
    let health = Health::new(api.clone(), watch.health(), watch.topology());
    let stamps = Stamps::new(api.clone(), watch.stamps());
    let swap = match market_rx {
        Some(rx) => Swap::new(watch.swap()).with_market_feed(rx),
        None => Swap::new(watch.swap()),
    };
    let lottery = Lottery::new(api.clone(), watch.health(), watch.lottery());
    let peers = Peers::new(api.clone(), watch.topology());
    let network = Network::new(watch.network(), watch.topology());
    let warmup = Warmup::new(watch.health(), watch.stamps(), watch.topology());
    let api_health = ApiHealth::new(
        api.clone(),
        watch.health(),
        watch.transactions(),
        log_capture::handle(),
    );
    let tags = Tags::new(watch.tags());
    let pins = Pins::new(api.clone(), watch.pins());
    let manifest = Manifest::new(api.clone());
    let watchlist = Watchlist::new();
    let feed_timeline = FeedTimeline::new();
    let pubsub_screen = Pubsub::new();
    let fleet = crate::components::fleet::Fleet::new(fleet_rx, api.name.clone(), fleet_resync_tx);
    vec![
        Box::new(health),
        Box::new(stamps),
        Box::new(swap),
        Box::new(lottery),
        Box::new(peers),
        Box::new(network),
        Box::new(warmup),
        Box::new(api_health),
        Box::new(tags),
        Box::new(pins),
        Box::new(manifest),
        Box::new(watchlist),
        Box::new(feed_timeline),
        Box::new(pubsub_screen),
        Box::new(fleet),
    ]
}

/// Build the 4104-byte (8 + 4096) synthetic chunk that
/// Translate a per-gate `Alert` to the notification center's
/// severity ladder. `Pass` outcomes are recoveries; `Warn` and
/// `Fail` keep their semantics. Pure — exposed so the test module
/// can pin every variant without setting up an `App`.
pub fn severity_from_alert(
    alert: &crate::alerts::Alert,
) -> crate::notifications::NotificationSeverity {
    use crate::components::health::GateStatus;
    use crate::notifications::NotificationSeverity;
    match alert.to {
        GateStatus::Fail => NotificationSeverity::Fail,
        GateStatus::Warn => NotificationSeverity::Warn,
        GateStatus::Pass => NotificationSeverity::Recovery,
        GateStatus::Unknown => NotificationSeverity::Info,
    }
}

/// Translate a fleet-aggregator transition entry to the
/// notification center's severity ladder. Mirrors
/// [`severity_from_alert`] but for the fleet view's per-row
/// transitions.
pub fn severity_from_fleet_entry(
    entry: &FleetAlertEntry,
) -> crate::notifications::NotificationSeverity {
    use crate::fleet::FleetStatus;
    use crate::notifications::NotificationSeverity;
    match entry.to {
        FleetStatus::Fail => NotificationSeverity::Fail,
        FleetStatus::Warn => NotificationSeverity::Warn,
        FleetStatus::Pass => NotificationSeverity::Recovery,
        FleetStatus::Unknown => NotificationSeverity::Info,
    }
}

/// `:probe-upload` ships at Bee. Timestamp-randomised so each
/// invocation produces a unique chunk address — Bee's
/// content-addressing dedup would otherwise short-circuit the
/// second probe on a fresh batch and skew the latency reading.
/// Returns `Vec<u8>`, which `bee::FileApi::upload_chunk` accepts via
/// its `impl Into<bytes::Bytes>` parameter.
fn build_synthetic_probe_chunk() -> Vec<u8> {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let mut data = Vec::with_capacity(8 + 4096);
    // Span: little-endian u64 with the payload length.
    data.extend_from_slice(&4096u64.to_le_bytes());
    // Payload: 16 bytes of timestamp + zero-padding to 4096.
    data.extend_from_slice(&nanos.to_le_bytes());
    data.resize(8 + 4096, 0);
    data
}

/// Truncate a hex string to a short prefix with an ellipsis. Used by
/// `:probe-upload` for the human-readable batch + reference labels.
fn short_hex(hex: &str, len: usize) -> String {
    if hex.len() > len {
        format!("{}…", &hex[..len])
    } else {
        hex.to_string()
    }
}

/// Best-effort MIME guess from the file extension. The cockpit's
/// `:upload-file` is the only caller; bee-rs falls back to
/// `application/octet-stream` if we hand it an empty string, but
/// recognising the common types saves operators from a manual
/// `--content-type` flag for typical web/document workflows.
fn guess_content_type(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html") | Some("htm") => "text/html",
        Some("txt") | Some("md") => "text/plain",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        Some("gz") | Some("tgz") => "application/gzip",
        Some("wasm") => "application/wasm",
        _ => "",
    }
    .to_string()
}

/// Build the closure the metrics HTTP handler invokes on each
/// scrape. Captures cloned `BeeWatch` receivers (cheap — they're
/// `Arc`-backed) plus the log-capture handle, then re-reads the
/// latest snapshot of each on every call. Returns an `Arc<Fn>`
/// matching `metrics_server::RenderFn`.
fn build_metrics_render_fn(
    watch: BeeWatch,
    log_capture: Option<log_capture::LogCapture>,
) -> crate::metrics_server::RenderFn {
    use std::time::{SystemTime, UNIX_EPOCH};
    Arc::new(move || {
        let health = watch.health().borrow().clone();
        let stamps = watch.stamps().borrow().clone();
        let swap = watch.swap().borrow().clone();
        let lottery = watch.lottery().borrow().clone();
        let topology = watch.topology().borrow().clone();
        let network = watch.network().borrow().clone();
        let transactions = watch.transactions().borrow().clone();
        let recent = log_capture
            .as_ref()
            .map(|c| c.snapshot())
            .unwrap_or_default();
        let call_stats = crate::components::api_health::call_stats_for(&recent);
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        let inputs = crate::metrics::MetricsInputs {
            bee_tui_version: env!("CARGO_PKG_VERSION"),
            health: &health,
            stamps: &stamps,
            swap: &swap,
            lottery: &lottery,
            topology: &topology,
            network: &network,
            transactions: &transactions,
            call_stats: &call_stats,
            now_unix,
        };
        crate::metrics::render(&inputs)
    })
}

fn format_gate_line(g: &Gate) -> String {
    let glyphs = crate::theme::active().glyphs;
    let glyph = match g.status {
        GateStatus::Pass => glyphs.pass,
        GateStatus::Warn => glyphs.warn,
        GateStatus::Fail => glyphs.fail,
        GateStatus::Unknown => glyphs.bullet,
    };
    let mut s = format!(
        "  [{glyph}] {label:<28} {value}\n",
        label = g.label,
        value = g.value
    );
    if let Some(why) = &g.why {
        s.push_str(&format!("        {} {why}\n", glyphs.continuation));
    }
    s
}

/// Strip scheme + host from a URL, leaving only the path + query.
/// Mirrors the redaction the S10 command-log pane applies on render.
fn path_only(url: &str) -> String {
    if let Some(idx) = url.find("//") {
        let after_scheme = &url[idx + 2..];
        if let Some(slash) = after_scheme.find('/') {
            return after_scheme[slash..].to_string();
        }
        return "/".into();
    }
    url.to_string()
}

/// Format the current wall-clock UTC time as `HH:MM:SS`. We compute
/// from `SystemTime::now()` directly so the binary stays free of a
/// chrono / time dep just for this one display string.
/// Append-write to `path`. Used by the `:pins-check` background task
/// to stream NDJSON-style results into a file the operator can
/// `tail -f`.
fn append(path: &PathBuf, s: &str) -> std::io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new().append(true).open(path)?;
    f.write_all(s.as_bytes())
}

/// Bee returns logger verbosity as a free-form string — usually
/// `"all"`, `"trace"`, `"debug"`, `"info"`, `"warning"`, `"error"`,
/// `"none"`, plus the legacy numeric forms `"1"`/`"2"`/`"3"`. Map to
/// a coarse rank so the noisier loggers sort to the top of the
/// `:loggers` dump. Unknown strings get rank 0 (silent end).
fn verbosity_rank(s: &str) -> u8 {
    match s {
        "all" | "trace" => 5,
        "debug" => 4,
        "info" | "1" => 3,
        "warning" | "warn" | "2" => 2,
        "error" | "3" => 1,
        _ => 0,
    }
}

/// Drop characters that are unsafe in a filename. Profile names come
/// from the user's `config.toml`, so we accept what's in there but
/// keep the path well-behaved on every shell.
fn sanitize_for_filename(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => c,
            _ => '-',
        })
        .collect()
}

/// Outcome of a `q` keystroke under the double-tap-to-quit guard.
/// Pure data so [`resolve_quit_press`] can be unit-tested without
/// any TUI / event-loop scaffolding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuitResolution {
    /// Second `q` arrived inside the confirmation window — quit.
    Confirm,
    /// First `q`, or a second `q` after the window expired —
    /// remember the timestamp and surface the hint.
    Pending,
}

/// Decide what to do with a `q` press given the previous press
/// timestamp (if any) and the current time. The window is supplied
/// rather than read from a constant so tests can use short windows
/// without sleeping.
fn resolve_quit_press(prev: Option<Instant>, now: Instant, window: Duration) -> QuitResolution {
    match prev {
        Some(t) if now.duration_since(t) <= window => QuitResolution::Confirm,
        _ => QuitResolution::Pending,
    }
}

fn format_utc_now() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let secs_in_day = secs % 86_400;
    let h = secs_in_day / 3_600;
    let m = (secs_in_day % 3_600) / 60;
    let s = secs_in_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_utc_now_returns_eight_chars() {
        let s = format_utc_now();
        assert_eq!(s.len(), 8);
        assert_eq!(s.chars().nth(2), Some(':'));
        assert_eq!(s.chars().nth(5), Some(':'));
    }

    #[test]
    fn path_only_strips_scheme_and_host() {
        assert_eq!(path_only("http://10.0.1.5:1633/status"), "/status");
        assert_eq!(
            path_only("https://bee.example.com/stamps?limit=10"),
            "/stamps?limit=10"
        );
    }

    #[test]
    fn path_only_handles_no_path() {
        assert_eq!(path_only("http://localhost:1633"), "/");
    }

    #[test]
    fn path_only_passes_relative_through() {
        assert_eq!(path_only("/already/relative"), "/already/relative");
    }

    #[test]
    fn parse_pprof_arg_default_60() {
        assert_eq!(parse_pprof_arg("diagnose --pprof"), Some(60));
        assert_eq!(parse_pprof_arg("diag --pprof some other"), Some(60));
    }

    #[test]
    fn parse_pprof_arg_with_explicit_seconds() {
        assert_eq!(parse_pprof_arg("diagnose --pprof=120"), Some(120));
        assert_eq!(parse_pprof_arg("diagnose --pprof=15 trailing"), Some(15));
    }

    #[test]
    fn parse_pprof_arg_clamps_extreme_values() {
        // 0 → 1 (lower clamp), 9999 → 600 (upper clamp).
        assert_eq!(parse_pprof_arg("diagnose --pprof=0"), Some(1));
        assert_eq!(parse_pprof_arg("diagnose --pprof=9999"), Some(600));
    }

    #[test]
    fn parse_pprof_arg_none_when_absent() {
        assert_eq!(parse_pprof_arg("diagnose"), None);
        assert_eq!(parse_pprof_arg("diag"), None);
        assert_eq!(parse_pprof_arg(""), None);
    }

    #[test]
    fn parse_pprof_arg_ignores_garbage_value() {
        // Garbage after `=` falls through to None — operator gets the
        // sync diagnostic, not a panic on bad input.
        assert_eq!(parse_pprof_arg("diagnose --pprof=lol"), None);
    }

    #[test]
    fn guess_content_type_known_extensions() {
        let p = std::path::PathBuf::from;
        assert_eq!(guess_content_type(&p("/tmp/x.html")), "text/html");
        assert_eq!(guess_content_type(&p("/tmp/x.json")), "application/json");
        assert_eq!(guess_content_type(&p("/tmp/x.PNG")), "image/png");
        assert_eq!(guess_content_type(&p("/tmp/x.tar.gz")), "application/gzip");
    }

    #[test]
    fn guess_content_type_unknown_returns_empty() {
        let p = std::path::PathBuf::from;
        // bee-rs treats empty as "use default application/octet-stream",
        // so an unknown extension shouldn't produce a misleading guess.
        assert_eq!(guess_content_type(&p("/tmp/x.unknownext")), "");
        assert_eq!(guess_content_type(&p("/tmp/no-extension")), "");
    }

    #[test]
    fn sanitize_for_filename_keeps_safe_chars() {
        assert_eq!(sanitize_for_filename("prod-1"), "prod-1");
        assert_eq!(sanitize_for_filename("lab_node"), "lab_node");
    }

    #[test]
    fn sanitize_for_filename_replaces_unsafe_chars() {
        assert_eq!(sanitize_for_filename("a/b\\c d"), "a-b-c-d");
        assert_eq!(sanitize_for_filename("name:colon"), "name-colon");
    }

    #[test]
    fn resolve_quit_press_first_press_is_pending() {
        let now = Instant::now();
        assert_eq!(
            resolve_quit_press(None, now, Duration::from_millis(1500)),
            QuitResolution::Pending
        );
    }

    #[test]
    fn resolve_quit_press_second_press_inside_window_confirms() {
        let first = Instant::now();
        let window = Duration::from_millis(1500);
        let second = first + Duration::from_millis(500);
        assert_eq!(
            resolve_quit_press(Some(first), second, window),
            QuitResolution::Confirm
        );
    }

    #[test]
    fn resolve_quit_press_second_press_after_window_resets_to_pending() {
        // A `q` long after the previous press should restart the
        // double-tap window — the operator hasn't really "meant it
        // twice in a row".
        let first = Instant::now();
        let window = Duration::from_millis(1500);
        let second = first + Duration::from_millis(2_000);
        assert_eq!(
            resolve_quit_press(Some(first), second, window),
            QuitResolution::Pending
        );
    }

    #[test]
    fn resolve_quit_press_at_window_boundary_confirms() {
        // Exactly at the boundary the press counts as confirm —
        // operators tapping in rhythm shouldn't be punished by jitter.
        let first = Instant::now();
        let window = Duration::from_millis(1500);
        let second = first + window;
        assert_eq!(
            resolve_quit_press(Some(first), second, window),
            QuitResolution::Confirm
        );
    }

    #[test]
    fn screen_keymap_covers_drill_screens() {
        // Stamps (1) and Peers (4) are the two screens with drill
        // panes — both must list ↑↓ / Enter / Esc in the help.
        for idx in [1usize, 4] {
            let rows = screen_keymap(idx);
            assert!(
                rows.iter().any(|(k, _)| k.contains("Enter")),
                "screen {idx} keymap must mention Enter (drill)"
            );
            assert!(
                rows.iter().any(|(k, _)| k.contains("Esc")),
                "screen {idx} keymap must mention Esc (close drill)"
            );
        }
    }

    #[test]
    fn screen_keymap_lottery_advertises_rchash() {
        let rows = screen_keymap(3);
        assert!(rows.iter().any(|(k, _)| k.contains("r")));
    }

    #[test]
    fn screen_keymap_unknown_index_is_empty_not_panic() {
        assert!(screen_keymap(999).is_empty());
    }

    #[test]
    fn verbosity_rank_orders_loud_to_silent() {
        assert!(verbosity_rank("all") > verbosity_rank("debug"));
        assert!(verbosity_rank("debug") > verbosity_rank("info"));
        assert!(verbosity_rank("info") > verbosity_rank("warning"));
        assert!(verbosity_rank("warning") > verbosity_rank("error"));
        assert!(verbosity_rank("error") > verbosity_rank("unknown"));
        // Numeric and named forms sort identically.
        assert_eq!(verbosity_rank("info"), verbosity_rank("1"));
        assert_eq!(verbosity_rank("warning"), verbosity_rank("2"));
    }

    #[test]
    fn filter_command_suggestions_empty_buffer_returns_all() {
        let matches = filter_command_suggestions("", KNOWN_COMMANDS);
        assert_eq!(matches.len(), KNOWN_COMMANDS.len());
    }

    #[test]
    fn filter_command_suggestions_prefix_matches_case_insensitive() {
        let matches = filter_command_suggestions("Bu", KNOWN_COMMANDS);
        let names: Vec<&str> = matches.iter().map(|(n, _)| *n).collect();
        assert!(names.contains(&"buy-preview"));
        assert!(names.contains(&"buy-suggest"));
        assert_eq!(names.len(), 2);
    }

    #[test]
    fn filter_command_suggestions_unknown_prefix_is_empty() {
        let matches = filter_command_suggestions("xyz", KNOWN_COMMANDS);
        assert!(matches.is_empty());
    }

    #[test]
    fn filter_command_suggestions_uses_first_token_only() {
        // `:topup-preview a1b2 1000` — the prefix is the verb, not
        // any of the args.
        let matches = filter_command_suggestions("topup-preview a1b2 1000", KNOWN_COMMANDS);
        let names: Vec<&str> = matches.iter().map(|(n, _)| *n).collect();
        assert_eq!(names, vec!["topup-preview"]);
    }

    #[test]
    fn resolve_command_line_expands_highlighted_suggestion() {
        // The bug: typing a prefix, arrowing the picker, then Enter
        // ran the half-typed prefix. Enter must run the *selected*
        // suggestion instead. `pe` filters to a single match,
        // `peers` — index 0 — so Enter resolves to the full verb.
        assert_eq!(resolve_command_line("pe", 0), "peers");
    }

    #[test]
    fn resolve_command_line_respects_the_selected_index() {
        // `pi` matches `pins` then `pins-check` (KNOWN_COMMANDS
        // order). Arrowing down to index 1 must resolve to the
        // second entry, not the first.
        assert_eq!(resolve_command_line("pi", 0), "pins");
        assert_eq!(resolve_command_line("pi", 1), "pins-check");
    }

    #[test]
    fn resolve_command_line_keeps_args_after_the_verb() {
        // A fully-typed command with args: the highlighted
        // suggestion supplies the verb, the rest of the buffer is
        // preserved as-is.
        assert_eq!(resolve_command_line("context lab", 0), "context lab");
    }

    #[test]
    fn resolve_command_line_falls_back_to_raw_buffer_when_no_match() {
        // No suggestion matches → return the buffer untouched so
        // `execute_command` can report it as unknown.
        assert_eq!(resolve_command_line("zzz-not-a-verb", 0), "zzz-not-a-verb");
    }

    #[test]
    fn probe_chunk_is_4104_bytes_with_correct_span() {
        // span(8) + payload(4096) = 4104, span = 4096 little-endian.
        let chunk = build_synthetic_probe_chunk();
        assert_eq!(chunk.len(), 4104);
        let span = u64::from_le_bytes(chunk[..8].try_into().unwrap());
        assert_eq!(span, 4096);
    }

    #[test]
    fn probe_chunk_payloads_are_unique_per_call() {
        // Timestamp-randomised → two consecutive builds must differ.
        // The randomness lives in payload bytes 0..16, so compare just
        // that window to keep the test deterministic against the
        // zero-padded tail.
        let a = build_synthetic_probe_chunk();
        // tiny sleep so the nanosecond clock is guaranteed to advance
        std::thread::sleep(Duration::from_micros(1));
        let b = build_synthetic_probe_chunk();
        assert_ne!(&a[8..24], &b[8..24]);
    }

    #[test]
    fn short_hex_truncates_with_ellipsis() {
        assert_eq!(short_hex("a1b2c3d4e5f6", 8), "a1b2c3d4…");
        assert_eq!(short_hex("short", 8), "short");
        assert_eq!(short_hex("abcdefgh", 8), "abcdefgh");
    }

    #[test]
    fn verb_category_covers_every_known_command() {
        // Every entry in KNOWN_COMMANDS must map to a real category
        // (never the "other" fall-through), otherwise the verb won't
        // appear in the help overlay's grouped list. Easy to forget
        // when adding a new verb.
        for (name, _) in KNOWN_COMMANDS {
            assert_ne!(
                verb_category(name),
                "other",
                "verb {name} has no category — add it to verb_category()"
            );
        }
    }

    #[test]
    fn verb_category_groups_known_verbs() {
        assert_eq!(verb_category("health"), "navigate");
        assert_eq!(verb_category("buy-suggest"), "stamps & economics");
        assert_eq!(verb_category("upload-file"), "uploads");
        assert_eq!(verb_category("manifest"), "inspect");
        assert_eq!(verb_category("watch-ref"), "durability");
        assert_eq!(verb_category("pubsub-pss"), "pubsub");
        assert_eq!(verb_category("nodes"), "cockpit");
        assert_eq!(verb_category("notifications"), "cockpit");
    }

    // --- v1.12 SupervisorWatchdog tests ---

    fn watchdog_with(policy: crate::config::BeeSupervisorConfig) -> SupervisorWatchdog {
        SupervisorWatchdog {
            bin: std::path::PathBuf::from("/usr/bin/true"),
            config: std::path::PathBuf::from("/dev/null"),
            logs: crate::config::BeeLogsConfig::default(),
            policy,
            restart_history: std::collections::VecDeque::new(),
            next_attempt_at: None,
            restart_count: 0,
        }
    }

    #[test]
    fn watchdog_should_attempt_returns_false_when_disabled() {
        let mut w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: false,
            max_restarts_per_hour: 6,
            backoff_initial_secs: 1,
            backoff_max_secs: 30,
        });
        assert!(!w.should_attempt(Instant::now()));
    }

    #[test]
    fn watchdog_should_attempt_respects_backoff_window() {
        let mut w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: true,
            max_restarts_per_hour: 6,
            backoff_initial_secs: 5,
            backoff_max_secs: 30,
        });
        let t0 = Instant::now();
        w.record_attempt(t0);
        // Immediately after recording, we should be in the backoff
        // window and not allowed to restart yet.
        assert!(!w.should_attempt(t0 + Duration::from_secs(2)));
        // After the backoff has elapsed, we are.
        assert!(w.should_attempt(t0 + Duration::from_secs(60)));
    }

    #[test]
    fn watchdog_should_attempt_respects_budget() {
        let mut w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: true,
            max_restarts_per_hour: 2,
            backoff_initial_secs: 1,
            backoff_max_secs: 30,
        });
        let t0 = Instant::now();
        w.record_attempt(t0);
        w.record_attempt(t0 + Duration::from_secs(60));
        // Budget hit; should refuse even past the backoff.
        assert!(!w.should_attempt(t0 + Duration::from_secs(1000)));
    }

    #[test]
    fn watchdog_backoff_grows_exponentially_capped() {
        let w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: true,
            max_restarts_per_hour: 100,
            backoff_initial_secs: 1,
            backoff_max_secs: 30,
        });
        assert_eq!(w.backoff_for(0), Duration::from_secs(1));
        assert_eq!(w.backoff_for(1), Duration::from_secs(2));
        assert_eq!(w.backoff_for(2), Duration::from_secs(4));
        assert_eq!(w.backoff_for(3), Duration::from_secs(8));
        assert_eq!(w.backoff_for(4), Duration::from_secs(16));
        // Capped at backoff_max_secs.
        assert_eq!(w.backoff_for(8), Duration::from_secs(30));
        assert_eq!(w.backoff_for(100), Duration::from_secs(30));
    }

    #[test]
    fn watchdog_history_slides_after_hour() {
        let mut w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: true,
            max_restarts_per_hour: 6,
            backoff_initial_secs: 1,
            backoff_max_secs: 30,
        });
        let t0 = Instant::now();
        w.record_attempt(t0);
        assert_eq!(w.restarts_last_hour(t0 + Duration::from_secs(10)), 1);
        // Slide forward past the one-hour mark — history should empty.
        assert_eq!(w.restarts_last_hour(t0 + Duration::from_secs(3700)), 0);
    }

    #[test]
    fn watchdog_top_bar_label_format() {
        let w = watchdog_with(crate::config::BeeSupervisorConfig {
            auto_restart: true,
            ..Default::default()
        });
        assert_eq!(
            w.top_bar_label(true, Some(Duration::from_secs(60 * 60 * 25))),
            "bee running 1d1h"
        );
        let mut w2 = w.clone();
        w2.restart_count = 2;
        assert_eq!(
            w2.top_bar_label(true, Some(Duration::from_secs(120))),
            "bee running 2m0s (2 restarts)"
        );
        let mut w3 = w.clone();
        w3.restart_count = 1;
        assert_eq!(
            w3.top_bar_label(true, Some(Duration::from_secs(5))),
            "bee running 5s (1 restart)"
        );
    }

    #[test]
    fn format_duration_short_thresholds() {
        assert_eq!(format_duration_short(Duration::from_secs(45)), "45s");
        assert_eq!(
            format_duration_short(Duration::from_secs(60 * 5 + 30)),
            "5m30s"
        );
        assert_eq!(
            format_duration_short(Duration::from_secs(3600 * 4 + 60 * 12)),
            "4h12m"
        );
        assert_eq!(
            format_duration_short(Duration::from_secs(86_400 * 3 + 3_600 * 5)),
            "3d5h"
        );
    }

    // --- v1.12 FleetAggregator tests ---

    fn fleet_row(name: &str, status: crate::fleet::FleetStatus) -> crate::fleet::FleetRow {
        crate::fleet::FleetRow {
            name: name.into(),
            url: format!("http://{name}"),
            default: false,
            status,
            peers: Some(50),
            worst_ttl_secs: Some(86_400 * 30),
            ping_ms: Some(10),
            warming_up: false,
            last_probe: Some(Instant::now()),
            why: match status {
                crate::fleet::FleetStatus::Fail => Some("unreachable".into()),
                crate::fleet::FleetStatus::Warn => Some("warming up".into()),
                _ => None,
            },
        }
    }

    fn fleet_snap(rows: Vec<crate::fleet::FleetRow>) -> crate::fleet::FleetSnapshot {
        crate::fleet::FleetSnapshot {
            rows,
            last_update: Some(Instant::now()),
        }
    }

    #[test]
    fn aggregator_pass_to_fail_is_buffered() {
        let mut agg = FleetAggregator::default();
        let now = Instant::now();
        // First snapshot establishes baseline (all Pass).
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now,
        );
        // Second snapshot flips to Fail.
        let added = agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Fail)]),
            now + Duration::from_secs(10),
        );
        assert_eq!(added, 1);
        assert_eq!(agg.pending.len(), 1);
        assert_eq!(agg.pending[0].to, crate::fleet::FleetStatus::Fail);
    }

    #[test]
    fn aggregator_unknown_transitions_are_ignored() {
        let mut agg = FleetAggregator::default();
        let now = Instant::now();
        // Cold-start: every row Unknown.
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Unknown)]),
            now,
        );
        // First real result: Pass. Unknown→Pass is not a transition
        // worth alerting on (it's just "we finally got an answer").
        let added = agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now + Duration::from_secs(10),
        );
        assert_eq!(added, 0);
        assert!(agg.pending.is_empty());
    }

    #[test]
    fn aggregator_steady_state_failure_does_not_re_alert() {
        let mut agg = FleetAggregator::default();
        let now = Instant::now();
        // Establish Pass baseline.
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now,
        );
        // Flip to Fail.
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Fail)]),
            now + Duration::from_secs(10),
        );
        // Steady Fail — should NOT add a second entry.
        let added = agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Fail)]),
            now + Duration::from_secs(20),
        );
        assert_eq!(added, 0);
        assert_eq!(agg.pending.len(), 1);
    }

    #[test]
    fn aggregator_recovery_is_alerted() {
        let mut agg = FleetAggregator::default();
        let now = Instant::now();
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now,
        );
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Fail)]),
            now + Duration::from_secs(10),
        );
        let added = agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now + Duration::from_secs(20),
        );
        assert_eq!(added, 1);
        assert!(
            agg.pending
                .last()
                .map(|e| e.from == crate::fleet::FleetStatus::Fail)
                .unwrap_or(false)
        );
    }

    #[test]
    fn aggregator_drain_waits_for_window() {
        let mut agg = FleetAggregator::default();
        let now = Instant::now();
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Pass)]),
            now,
        );
        agg.ingest_snapshot(
            &fleet_snap(vec![fleet_row("a", crate::fleet::FleetStatus::Fail)]),
            now + Duration::from_secs(10),
        );
        // Before the window: returns None.
        assert!(
            agg.drain_if_window_elapsed(now + Duration::from_secs(20), Duration::from_secs(60),)
                .is_none()
        );
        // After the window: returns the buffered entries.
        let drained = agg
            .drain_if_window_elapsed(now + Duration::from_secs(120), Duration::from_secs(60))
            .expect("window elapsed");
        assert_eq!(drained.len(), 1);
        assert!(agg.pending.is_empty());
        assert!(agg.window_opened_at.is_none());
    }

    #[test]
    fn aggregator_format_message_consolidates_counts() {
        let entries = vec![
            FleetAlertEntry {
                node: "prod-eu".into(),
                from: crate::fleet::FleetStatus::Pass,
                to: crate::fleet::FleetStatus::Fail,
                why: Some("unreachable".into()),
            },
            FleetAlertEntry {
                node: "prod-us".into(),
                from: crate::fleet::FleetStatus::Pass,
                to: crate::fleet::FleetStatus::Warn,
                why: Some("only 2 peers (< 4)".into()),
            },
        ];
        let msg = FleetAggregator::format_message(&entries);
        assert!(msg.starts_with("Fleet alert: 1 fail · 1 warn"));
        assert!(msg.contains("prod-eu"));
        assert!(msg.contains("unreachable"));
        assert!(msg.contains("prod-us"));
    }

    // --- v1.12 BatchAction tests ---

    #[test]
    fn batch_action_from_char_is_case_insensitive() {
        assert_eq!(BatchAction::from_char('t'), Some(BatchAction::Topup));
        assert_eq!(BatchAction::from_char('T'), Some(BatchAction::Topup));
        assert_eq!(BatchAction::from_char('d'), Some(BatchAction::Dilute));
        assert_eq!(BatchAction::from_char('p'), Some(BatchAction::Plan));
        assert_eq!(BatchAction::from_char('x'), None);
    }

    #[test]
    fn batch_action_fields_match_verb_arg_counts() {
        // Each action's field list must match the # of positional
        // args the corresponding verb parser expects (otherwise the
        // modal will assemble an underfull or overfull line and the
        // verb errors at runtime, not at the form boundary).
        assert_eq!(BatchAction::Topup.fields().len(), 2);
        assert_eq!(BatchAction::Dilute.fields().len(), 2);
        assert_eq!(BatchAction::Extend.fields().len(), 2);
        assert_eq!(BatchAction::Buy.fields().len(), 2);
        assert_eq!(BatchAction::Plan.fields().len(), 1);
    }

    // --- v1.14 notification severity translation tests ---

    #[test]
    fn severity_from_alert_maps_every_gate_status() {
        use crate::alerts::Alert;
        use crate::components::health::GateStatus;
        use crate::notifications::NotificationSeverity;
        let mk = |to: GateStatus| Alert {
            gate: "ChainConnected".into(),
            from: GateStatus::Pass,
            to,
            value: "test".into(),
            why: None,
        };
        assert_eq!(
            severity_from_alert(&mk(GateStatus::Fail)),
            NotificationSeverity::Fail
        );
        assert_eq!(
            severity_from_alert(&mk(GateStatus::Warn)),
            NotificationSeverity::Warn
        );
        assert_eq!(
            severity_from_alert(&mk(GateStatus::Pass)),
            NotificationSeverity::Recovery
        );
        assert_eq!(
            severity_from_alert(&mk(GateStatus::Unknown)),
            NotificationSeverity::Info
        );
    }

    #[test]
    fn severity_from_fleet_entry_maps_every_fleet_status() {
        use crate::fleet::FleetStatus;
        use crate::notifications::NotificationSeverity;
        let mk = |to: FleetStatus| FleetAlertEntry {
            node: "prod-eu".into(),
            from: FleetStatus::Pass,
            to,
            why: None,
        };
        assert_eq!(
            severity_from_fleet_entry(&mk(FleetStatus::Fail)),
            NotificationSeverity::Fail
        );
        assert_eq!(
            severity_from_fleet_entry(&mk(FleetStatus::Warn)),
            NotificationSeverity::Warn
        );
        assert_eq!(
            severity_from_fleet_entry(&mk(FleetStatus::Pass)),
            NotificationSeverity::Recovery
        );
        assert_eq!(
            severity_from_fleet_entry(&mk(FleetStatus::Unknown)),
            NotificationSeverity::Info
        );
    }
}
