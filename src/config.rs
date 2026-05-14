#![allow(dead_code)] // Remove this once you start using the code

use std::{
    collections::{HashMap, HashSet},
    env,
    path::PathBuf,
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use directories::ProjectDirs;
use lazy_static::lazy_static;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, de::Deserializer};
use tracing::error;

use crate::{action::Action, app::Mode};

const CONFIG: &str = include_str!("../.config/config.json5");

#[derive(Clone, Debug, Deserialize, Default)]
pub struct AppConfig {
    #[serde(default)]
    pub data_dir: PathBuf,
    #[serde(default)]
    pub config_dir: PathBuf,
}

/// One configured Bee node. Multiple may coexist; multi-node UX is
/// targeted at v0.4 but the schema supports it from day one.
#[derive(Clone, Debug, Deserialize)]
pub struct NodeConfig {
    /// Friendly label shown in the UI (e.g. `"prod-1"`, `"local"`).
    pub name: String,
    /// Bee API base URL (e.g. `"http://localhost:1633"`).
    pub url: String,
    /// Optional bearer token for restricted-mode nodes. Supports the
    /// `@env:VAR_NAME` indirection — see [`NodeConfig::resolved_token`].
    #[serde(default)]
    pub token: Option<String>,
    /// Optional path to this node's log file. When set and bee-tui is
    /// *not* spawning Bee itself (no `[bee]` block / `--bee-bin`), the
    /// cockpit tails this file to populate the bottom log pane's
    /// Bee-side tabs (Errors / Warn / Info / Debug / Bee HTTP). Tailing
    /// starts at end-of-file — pre-existing history is not replayed.
    /// Ignored when bee-tui owns the supervisor (the supervised child's
    /// captured log is tailed instead).
    #[serde(default)]
    pub log_file: Option<PathBuf>,
    /// Optional shell command whose stdout streams this node's log —
    /// e.g. `journalctl -u bee -f`, `docker logs -f bee 2>&1`,
    /// `ssh host 'tail -f /var/log/bee.log'`. Run via `sh -c`, so
    /// pipes / quoting / redirects work. Lets bee-tui surface logs
    /// for a node whose log *file* it can't read directly (remote
    /// host, container, restricted permissions). Takes precedence
    /// over `log_file` when both are set. Same supervisor caveat as
    /// `log_file` — ignored when bee-tui spawns Bee itself.
    #[serde(default)]
    pub log_command: Option<String>,
    /// Marks the default profile loaded on startup. If no entry has
    /// `default = true`, the first node in the list is used.
    #[serde(default)]
    pub default: bool,
}

impl NodeConfig {
    /// Resolve `token` to its concrete value: `Some(env_var)` if the
    /// configured value starts with `@env:`, otherwise the literal.
    pub fn resolved_token(&self) -> Option<String> {
        let raw = self.token.as_deref()?;
        if let Some(var) = raw.strip_prefix("@env:") {
            env::var(var).ok()
        } else {
            Some(raw.to_string())
        }
    }
}

#[derive(Clone, Debug, Default, Deserialize)]
pub struct Config {
    #[serde(default, flatten)]
    pub config: AppConfig,
    #[serde(default = "default_nodes")]
    pub nodes: Vec<NodeConfig>,
    #[serde(default)]
    pub keybindings: KeyBindings,
    #[serde(default)]
    pub styles: Styles,
    /// `[ui]` section — theme + ascii-fallback knobs.
    #[serde(default)]
    pub ui: UiConfig,
    /// `[bee]` section — when present, bee-tui spawns the Bee node
    /// itself before opening the cockpit. Absence keeps the legacy
    /// behavior of connecting to an already-running Bee.
    #[serde(default)]
    pub bee: Option<BeeConfig>,
    /// `[metrics]` section — when present and `enabled = true`,
    /// bee-tui exposes a Prometheus `/metrics` endpoint on the
    /// configured address. Default off because exposing an HTTP
    /// listener should be an explicit operator opt-in.
    #[serde(default)]
    pub metrics: MetricsConfig,
    /// `[economics]` section — optional cost-context oracles
    /// (xBZZ → USD price + Gnosis chain gas). The `:price` verb
    /// works without configuration (uses Swarm's public token
    /// service); `:basefee` requires `gnosis_rpc_url` to be set.
    #[serde(default)]
    pub economics: EconomicsConfig,
    /// `[alerts]` section — webhook ping when a health gate flips.
    /// Disabled when `webhook_url` is absent (the default).
    #[serde(default)]
    pub alerts: AlertsConfig,
    /// `[durability]` section — knobs for `:durability-check` and
    /// `:watch-ref`. Defaults preserve v1.6 behaviour (no swarmscan
    /// cross-check, BMT verification on); operators opt in to the
    /// independent network probe explicitly.
    #[serde(default)]
    pub durability: DurabilityConfig,
    /// `[pubsub]` section — optional history-file writer for the
    /// S15 Pubsub watch live tail. Off by default; setting
    /// `history_file` to a path turns on JSONL append-on-arrival
    /// for every PSS / GSOC message.
    #[serde(default)]
    pub pubsub: PubsubConfig,
    /// `[fleet]` section — fleet-aggregate webhook. Off by default
    /// (per-node `[alerts]` keeps firing). When `aggregate_webhook_url`
    /// is set, on each fleet-poll tick bee-tui consolidates new
    /// Warn/Fail status across nodes into one POST per
    /// `aggregate_window_secs`.
    #[serde(default)]
    pub fleet: FleetConfig,
    /// `[notifications]` section — in-cockpit notification center.
    /// Toast overlay + history are on by default; desktop +
    /// terminal-bell escalations are opt-in.
    #[serde(default)]
    pub notifications: NotificationsConfig,
}

/// `[bee]` table from `config.toml`. Both fields are required so a
/// malformed `[bee]` block fails parse rather than silently spawning
/// nothing.
#[derive(Clone, Debug, Deserialize)]
pub struct BeeConfig {
    /// Path to the `bee` binary. Resolved relative to the working
    /// directory if not absolute — operators usually run bee-tui from
    /// the same shell they used to test the binary, so this is the
    /// least surprising behavior.
    pub bin: PathBuf,
    /// Path to the Bee YAML config file the binary should be started
    /// with. Same relative-to-cwd resolution as `bin`.
    pub config: PathBuf,
    /// `[bee.logs]` subsection — log rotation knobs. Both fields
    /// optional; an absent `[bee.logs]` keeps defaults of 64 MiB
    /// rotation at 5 retained files (~320 MiB ceiling).
    #[serde(default)]
    pub logs: BeeLogsConfig,
    /// `[bee.supervisor]` subsection — auto-restart policy applied
    /// when bee-tui acts as Bee's parent (`[bee].bin` set). Absent
    /// block keeps the v1.11 behaviour: log the crash, dim the top
    /// bar chip, no restart.
    #[serde(default)]
    pub supervisor: BeeSupervisorConfig,
}

/// `[bee.supervisor]` table. Off by default — pre-v1.12 behaviour
/// was "single-shot, no restart". Setting `auto_restart = true`
/// turns on the watchdog with exponential backoff and a per-hour
/// budget; everything else has a sensible default.
#[derive(Clone, Debug, Deserialize)]
pub struct BeeSupervisorConfig {
    /// When `true`, bee-tui re-spawns Bee after the child exits
    /// (any reason — clean exit, signal, OOM kill). When `false`
    /// (default), the supervisor goes dim and reports the exit;
    /// operators restart bee-tui to try again.
    #[serde(default)]
    pub auto_restart: bool,
    /// Maximum restarts allowed within a rolling one-hour window.
    /// Sliding budget that protects against restart storms (e.g.
    /// Bee crashes on startup, gets relaunched immediately, crashes
    /// again, ...). Default 6 — generous enough for "bad afternoon"
    /// but tight enough that an unrecoverable failure stops within
    /// a few minutes. Once exceeded, the watchdog stops respawning
    /// until the window slides forward.
    #[serde(default = "default_max_restarts_per_hour")]
    pub max_restarts_per_hour: u32,
    /// Initial backoff in seconds; doubles after each restart up to
    /// `backoff_max_secs`. Default 1 — feels live for transient
    /// failures, doesn't hammer the OS on fast crashloops.
    #[serde(default = "default_backoff_initial_secs")]
    pub backoff_initial_secs: u64,
    /// Cap on the exponential backoff. Default 30 s — long enough
    /// that an operator-investigating-a-bad-deploy can see the
    /// restart is paused, short enough that recovery doesn't take
    /// minutes after a transient issue resolves.
    #[serde(default = "default_backoff_max_secs")]
    pub backoff_max_secs: u64,
}

impl Default for BeeSupervisorConfig {
    fn default() -> Self {
        Self {
            auto_restart: false,
            max_restarts_per_hour: default_max_restarts_per_hour(),
            backoff_initial_secs: default_backoff_initial_secs(),
            backoff_max_secs: default_backoff_max_secs(),
        }
    }
}

fn default_max_restarts_per_hour() -> u32 {
    6
}
fn default_backoff_initial_secs() -> u64 {
    1
}
fn default_backoff_max_secs() -> u64 {
    30
}

/// `[bee.logs]` table from `config.toml`. Bounds the size of the
/// supervised Bee process's captured stdout+stderr file so a
/// long-running node doesn't fill `$TMPDIR`.
#[derive(Clone, Debug, Deserialize)]
pub struct BeeLogsConfig {
    /// Active log file rolls over once it reaches this many MiB.
    /// Default 64 MiB — large enough that operator-relevant traces
    /// fit in the live file, small enough that rotation happens
    /// within a day or two on a busy node.
    #[serde(default = "default_rotate_size_mb")]
    pub rotate_size_mb: u64,
    /// How many rotated files (`.1` .. `.N`) to retain. Default 5.
    /// At the 64 MiB default that's ~320 MiB of log history kept on
    /// disk; older content is unlinked.
    #[serde(default = "default_keep_files")]
    pub keep_files: u32,
}

impl Default for BeeLogsConfig {
    fn default() -> Self {
        Self {
            rotate_size_mb: default_rotate_size_mb(),
            keep_files: default_keep_files(),
        }
    }
}

fn default_rotate_size_mb() -> u64 {
    64
}
fn default_keep_files() -> u32 {
    5
}

/// `[metrics]` table from `config.toml`. Off by default — a
/// Prometheus endpoint is a network-facing surface, even if it
/// binds to localhost, so we make it a deliberate opt-in.
#[derive(Clone, Debug, Deserialize)]
pub struct MetricsConfig {
    /// Master switch. `false` skips spawning the HTTP server
    /// entirely.
    #[serde(default)]
    pub enabled: bool,
    /// Bind address. Defaults to localhost; an operator who
    /// genuinely wants `0.0.0.0` exposure has to type it.
    #[serde(default = "default_metrics_addr")]
    pub addr: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            addr: default_metrics_addr(),
        }
    }
}

fn default_metrics_addr() -> String {
    "127.0.0.1:9101".into()
}

/// `[economics]` table from `config.toml`. Optional cost-context
/// oracles. Both fields have sensible defaults so omitting the
/// table entirely is fine (the verbs gracefully degrade).
#[derive(Clone, Debug, Default, Deserialize)]
pub struct EconomicsConfig {
    /// JSON-RPC endpoint that the `:basefee` verb queries for
    /// Gnosis-chain gas pricing. Typically the same URL as Bee's
    /// `--blockchain-rpc-endpoint`. When unset, `:basefee` errors
    /// with a clear "configure [economics].gnosis_rpc_url" hint.
    #[serde(default)]
    pub gnosis_rpc_url: Option<String>,
    /// When `true`, S3 SWAP renders an always-on Market tile that
    /// polls xBZZ → USD every 60 s and (if `gnosis_rpc_url` is set)
    /// Gnosis basefee + tip alongside it. Off by default — fresh
    /// installs make no outbound traffic without an explicit opt-in.
    #[serde(default)]
    pub enable_market_tile: bool,
}

/// `[durability]` table from `config.toml`. Knobs for the chunk-graph
/// walker that powers `:durability-check` + `:watch-ref`. All fields
/// optional; defaults preserve v1.6 behaviour.
#[derive(Clone, Debug, Deserialize)]
pub struct DurabilityConfig {
    /// When `true`, every completed durability walk probes
    /// `swarmscan_url` for an independent "does the network see
    /// this ref" answer. The result lands on
    /// `DurabilityResult.swarmscan_seen` and surfaces in the
    /// summary line + S13 Watchlist row. Off by default — fresh
    /// installs make no outbound traffic to a third-party indexer.
    #[serde(default)]
    pub swarmscan_check: bool,
    /// URL template the swarmscan probe hits. The literal
    /// `{ref}` substring is replaced with the hex-encoded
    /// reference at request time. Any indexer with a similar
    /// shape (200 = seen, 404 = not seen) can be used. Defaults
    /// to swarmscan's public chunk endpoint.
    #[serde(default = "default_swarmscan_url")]
    pub swarmscan_url: String,
}

impl Default for DurabilityConfig {
    fn default() -> Self {
        Self {
            swarmscan_check: false,
            swarmscan_url: default_swarmscan_url(),
        }
    }
}

fn default_swarmscan_url() -> String {
    "https://api.swarmscan.io/v1/chunks/{ref}".into()
}

/// `[pubsub]` table from `config.toml`. Off by default — fresh
/// installs don't write any pubsub messages to disk. Setting
/// `history_file` to a path turns on append-on-arrival JSONL
/// logging of every delivered PSS / GSOC frame, useful for
/// offline analysis of overnight subscriptions. When the history
/// file is enabled, rotation keeps disk usage bounded — the active
/// file rolls over to `<path>.1` once it crosses `rotate_size_mb`,
/// and only the most-recent `keep_files` rotations are retained.
#[derive(Clone, Debug, Deserialize)]
pub struct PubsubConfig {
    /// Path to a JSONL file that bee-tui appends to whenever a
    /// pubsub frame arrives. The file is created with mode 0600
    /// (owner read/write only) so payloads can't accidentally be
    /// world-readable on a multi-user host. Each line is one
    /// JSON object with the same shape `--once feed-probe`'s
    /// data field uses.
    #[serde(default)]
    pub history_file: Option<PathBuf>,
    /// Active history file rolls over once it reaches this many
    /// MiB. Default 64 MiB. Zero disables rotation (file grows
    /// unbounded — operator's responsibility to truncate).
    #[serde(default = "default_pubsub_rotate_size_mb")]
    pub rotate_size_mb: u64,
    /// How many rotated history files (`<path>.1` .. `<path>.N`)
    /// to retain. Default 5. At the 64 MiB default that's a
    /// ~320 MiB ceiling; older rotations are unlinked.
    #[serde(default = "default_pubsub_keep_files")]
    pub keep_files: u32,
}

impl Default for PubsubConfig {
    fn default() -> Self {
        Self {
            history_file: None,
            rotate_size_mb: default_pubsub_rotate_size_mb(),
            keep_files: default_pubsub_keep_files(),
        }
    }
}

fn default_pubsub_rotate_size_mb() -> u64 {
    64
}

fn default_pubsub_keep_files() -> u32 {
    5
}

/// `[alerts]` table from `config.toml`. Off by default — without a
/// `webhook_url`, the alerter is a no-op. The debounce knob exists
/// so a flapping gate doesn't pin an operator's Slack channel.
#[derive(Clone, Debug, Deserialize)]
pub struct AlertsConfig {
    /// Slack/Discord-compatible incoming-webhook URL. When absent
    /// (the default), no alerts are sent.
    #[serde(default)]
    pub webhook_url: Option<String>,
    /// Per-gate debounce window in seconds. After firing for gate X,
    /// no further alert for X until this elapses regardless of how
    /// many times the gate flapped. Default 300 (5 min).
    #[serde(default = "default_alerts_debounce_secs")]
    pub debounce_secs: u64,
}

impl Default for AlertsConfig {
    fn default() -> Self {
        Self {
            webhook_url: None,
            debounce_secs: default_alerts_debounce_secs(),
        }
    }
}

fn default_alerts_debounce_secs() -> u64 {
    crate::alerts::DEFAULT_DEBOUNCE_SECS
}

/// `[fleet]` table from `config.toml`. Off by default — the S15
/// Fleet screen works regardless of whether this is configured;
/// the only thing this section enables is the aggregate webhook
/// that consolidates per-node alerts across the fleet.
#[derive(Clone, Debug, Deserialize)]
pub struct FleetConfig {
    /// Slack / Discord-compatible incoming-webhook URL. When unset
    /// (default), no fleet-aggregate alerts are sent and each
    /// node's individual `[alerts].webhook_url` keeps working
    /// untouched. When set, on each fleet-poll tick bee-tui buffers
    /// new `Warn` / `Fail` status entries and fires ONE POST per
    /// `aggregate_window_secs` consolidating the buffered states.
    #[serde(default)]
    pub aggregate_webhook_url: Option<String>,
    /// Coalesce window for fleet-aggregate webhooks, in seconds.
    /// During this window, all node-status changes are batched
    /// into a single message. Default 60 — long enough that a
    /// transient network blip across three nodes folds into one
    /// alert, short enough that a real outage doesn't sit silent.
    #[serde(default = "default_fleet_window_secs")]
    pub aggregate_window_secs: u64,
}

impl Default for FleetConfig {
    fn default() -> Self {
        Self {
            aggregate_webhook_url: None,
            aggregate_window_secs: default_fleet_window_secs(),
        }
    }
}

fn default_fleet_window_secs() -> u64 {
    60
}

/// `[notifications]` table — v1.14 in-cockpit notification center.
/// Three layers, each independently configurable:
/// 1. `toast_enabled` (default `true`) shows a transient top-right
///    overlay for `toast_seconds` (default 8).
/// 2. `desktop` (default `false`) fires libnotify-style OS
///    notifications via `notify-rust` (zbus on Linux, no system lib
///    dep). Operators in a tmux pane / hidden window can see
///    cockpit events without flipping focus.
/// 3. `bell` (`"off"` / `"fail"` / `"warn"`, default `"off"`) emits
///    a terminal BEL on the matching severities. Loud — opt-in
///    only.
///
/// The notification history overlay (`Ctrl+Alt+N` / `:notifications`)
/// is always available; nothing in this section turns it off.
#[derive(Clone, Debug, Deserialize)]
pub struct NotificationsConfig {
    /// In-cockpit transient toast in the top-right corner. Default
    /// `true` — the toast is the operator-attention surface the
    /// cockpit didn't have before v1.14. Set `false` if you find
    /// it intrusive; the history overlay still records everything.
    #[serde(default = "default_toast_enabled")]
    pub toast_enabled: bool,
    /// How long (seconds) a toast stays on screen before auto-
    /// dismissing. Default 8 — long enough to read, short enough
    /// to not clutter mid-workflow. Clamped to ≥1 at runtime.
    #[serde(default = "default_toast_seconds")]
    pub toast_seconds: u64,
    /// Fire a libnotify / OS-level notification for Fail / Warn
    /// events. Default `false` — OS notifications are intrusive
    /// and operators in a busy environment shouldn't get them
    /// unsolicited. When set, errors (no dbus session, etc.)
    /// log a single warning and the rest of the pipeline keeps
    /// running.
    #[serde(default)]
    pub desktop: bool,
    /// Terminal-bell threshold. `"off"` (default), `"fail"` (BEL
    /// on Fail only), `"warn"` (BEL on Fail + Warn). Anything else
    /// is treated as `"off"`. Recovery events never ring the bell
    /// — operators don't want their terminal flashing on the
    /// happy news.
    #[serde(default = "default_bell")]
    pub bell: String,
}

impl Default for NotificationsConfig {
    fn default() -> Self {
        Self {
            toast_enabled: default_toast_enabled(),
            toast_seconds: default_toast_seconds(),
            desktop: false,
            bell: default_bell(),
        }
    }
}

fn default_toast_enabled() -> bool {
    true
}
fn default_toast_seconds() -> u64 {
    8
}
fn default_bell() -> String {
    "off".into()
}

/// `[ui]` table from `config.toml`. Every field has a sensible
/// default so the entire section can be omitted without breaking
/// startup.
#[derive(Clone, Debug, Default, Deserialize)]
pub struct UiConfig {
    /// Theme name. Recognised values: `"default"`, `"mono"`. Anything
    /// else falls back to the default theme with a warning logged.
    #[serde(default = "default_theme")]
    pub theme: String,
    /// When set, screens fall back to ASCII-only glyphs (✓ → `[X]`)
    /// for terminals that don't render Unicode reliably. Not yet
    /// wired through every component; reserved for follow-up.
    #[serde(default)]
    pub ascii_fallback: bool,
    /// Polling-cadence preset. Recognised values:
    /// - `"live"` — original 2 s health / 5 s topology+tags / 30 s swap+lottery+transactions / 60 s network. Most chatty; useful when actively diagnosing.
    /// - `"default"` — calmer (4 s health / 10 s topology+tags / 30 s mid tier / 60 s network). About half the request volume of `live`. The default for new installs since the bottom log pane was tabbed.
    /// - `"slow"` — minimal (8 s / 20 s / 60 s / 120 s). For "leave it open all day" monitoring.
    ///
    /// Unknown values fall back to `default` with a tracing warning.
    #[serde(default = "default_refresh")]
    pub refresh: String,
}

fn default_theme() -> String {
    "default".into()
}

fn default_refresh() -> String {
    "default".into()
}

impl Config {
    /// Pick the active node profile: first entry with `default = true`,
    /// otherwise the first entry, otherwise [`None`].
    pub fn active_node(&self) -> Option<&NodeConfig> {
        self.nodes
            .iter()
            .find(|n| n.default)
            .or_else(|| self.nodes.first())
    }
}

/// Default node list when the user hasn't configured any: a single
/// `local` profile pointing at `http://localhost:1633`.
fn default_nodes() -> Vec<NodeConfig> {
    vec![NodeConfig {
        name: "local".to_string(),
        url: "http://localhost:1633".to_string(),
        token: None,
        log_file: None,
        log_command: None,
        default: true,
    }]
}

lazy_static! {
    pub static ref PROJECT_NAME: String = env!("CARGO_CRATE_NAME").to_uppercase().to_string();
    pub static ref DATA_FOLDER: Option<PathBuf> =
        env::var(format!("{}_DATA", PROJECT_NAME.clone()))
            .ok()
            .map(PathBuf::from);
    pub static ref CONFIG_FOLDER: Option<PathBuf> =
        env::var(format!("{}_CONFIG", PROJECT_NAME.clone()))
            .ok()
            .map(PathBuf::from);
}

impl Config {
    pub fn new() -> color_eyre::Result<Self, config::ConfigError> {
        let default_config: Config = json5::from_str(CONFIG).unwrap();
        let data_dir = get_data_dir();
        let config_dir = get_config_dir();
        let mut builder = config::Config::builder()
            .set_default("data_dir", data_dir.to_str().unwrap())?
            .set_default("config_dir", config_dir.to_str().unwrap())?;

        let search_dirs = config_search_dirs();
        let mut found_config = false;
        'search: for dir in &search_dirs {
            for (file, format) in &CONFIG_FILE_CANDIDATES {
                let path = dir.join(file);
                if path.exists() {
                    builder = builder
                        .add_source(config::File::from(path).format(*format).required(false));
                    found_config = true;
                    break 'search;
                }
            }
        }
        if !found_config {
            error!(
                "No configuration file found. Searched: {}. Application may not behave as expected",
                search_dirs
                    .iter()
                    .map(|d| d.display().to_string())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        let mut cfg: Self = builder.build()?.try_deserialize()?;

        for (mode, default_bindings) in default_config.keybindings.0.iter() {
            let user_bindings = cfg.keybindings.0.entry(*mode).or_default();
            for (key, cmd) in default_bindings.iter() {
                user_bindings
                    .entry(key.clone())
                    .or_insert_with(|| cmd.clone());
            }
        }
        for (mode, default_styles) in default_config.styles.0.iter() {
            let user_styles = cfg.styles.0.entry(*mode).or_default();
            for (style_key, style) in default_styles.iter() {
                user_styles.entry(style_key.clone()).or_insert(*style);
            }
        }

        Ok(cfg)
    }
}

pub fn get_data_dir() -> PathBuf {
    if let Some(s) = DATA_FOLDER.clone() {
        s
    } else if let Some(proj_dirs) = project_directory() {
        proj_dirs.data_local_dir().to_path_buf()
    } else {
        PathBuf::from(".").join(".data")
    }
}

/// Config file names bee-tui recognises, in precedence order. The first
/// one present in a search directory wins.
const CONFIG_FILE_CANDIDATES: [(&str, config::FileFormat); 5] = [
    ("config.json5", config::FileFormat::Json5),
    ("config.json", config::FileFormat::Json),
    ("config.yaml", config::FileFormat::Yaml),
    ("config.toml", config::FileFormat::Toml),
    ("config.ini", config::FileFormat::Ini),
];

/// The platform-native config directory: XDG on Linux, `Application
/// Support` on macOS, Known Folders on Windows. Last-resort entry in
/// [`config_search_dirs`].
fn platform_config_dir() -> PathBuf {
    if let Some(proj_dirs) = project_directory() {
        proj_dirs.config_local_dir().to_path_buf()
    } else {
        PathBuf::from(".").join(".config")
    }
}

fn dedup_dirs(dirs: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    dirs.into_iter()
        .filter(|d| seen.insert(d.clone()))
        .collect()
}

/// Ordered list of directories searched for a config file. The first
/// directory that holds a recognised `config.*` file wins. `~/.config/
/// bee-tui` is searched on *every* platform — so macOS and Windows devs
/// don't have to hunt down the platform-native path.
pub fn config_search_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(explicit) = CONFIG_FOLDER.clone() {
        dirs.push(explicit);
    }
    if let Some(base) = directories::BaseDirs::new() {
        dirs.push(base.home_dir().join(".config").join("bee-tui"));
    }
    dirs.push(platform_config_dir());
    dedup_dirs(dirs)
}

/// The directory a config file was actually found in — the first entry
/// of [`config_search_dirs`] that contains a recognised config file.
/// `None` when no config file exists anywhere on the search path.
pub fn resolved_config_dir() -> Option<PathBuf> {
    config_search_dirs().into_iter().find(|dir| {
        CONFIG_FILE_CANDIDATES
            .iter()
            .any(|(file, _)| dir.join(file).exists())
    })
}

/// The config directory bee-tui uses: the resolved one if a config file
/// exists, otherwise the explicit `BEE_TUI_CONFIG` override, otherwise
/// the platform-native default.
pub fn get_config_dir() -> PathBuf {
    resolved_config_dir()
        .or_else(|| CONFIG_FOLDER.clone())
        .unwrap_or_else(platform_config_dir)
}

fn project_directory() -> Option<ProjectDirs> {
    ProjectDirs::from("com", "ethswarm-tools", env!("CARGO_PKG_NAME"))
}

#[derive(Clone, Debug, Default)]
pub struct KeyBindings(pub HashMap<Mode, HashMap<Vec<KeyEvent>, Action>>);

impl<'de> Deserialize<'de> for KeyBindings {
    fn deserialize<D>(deserializer: D) -> color_eyre::Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed_map = HashMap::<Mode, HashMap<String, Action>>::deserialize(deserializer)?;

        let keybindings = parsed_map
            .into_iter()
            .map(|(mode, inner_map)| {
                let converted_inner_map = inner_map
                    .into_iter()
                    .map(|(key_str, cmd)| (parse_key_sequence(&key_str).unwrap(), cmd))
                    .collect();
                (mode, converted_inner_map)
            })
            .collect();

        Ok(KeyBindings(keybindings))
    }
}

fn parse_key_event(raw: &str) -> color_eyre::Result<KeyEvent, String> {
    let raw_lower = raw.to_ascii_lowercase();
    let (remaining, modifiers) = extract_modifiers(&raw_lower);
    parse_key_code_with_modifiers(remaining, modifiers)
}

fn extract_modifiers(raw: &str) -> (&str, KeyModifiers) {
    let mut modifiers = KeyModifiers::empty();
    let mut current = raw;

    loop {
        match current {
            rest if rest.starts_with("ctrl-") => {
                modifiers.insert(KeyModifiers::CONTROL);
                current = &rest[5..];
            }
            rest if rest.starts_with("alt-") => {
                modifiers.insert(KeyModifiers::ALT);
                current = &rest[4..];
            }
            rest if rest.starts_with("shift-") => {
                modifiers.insert(KeyModifiers::SHIFT);
                current = &rest[6..];
            }
            _ => break, // break out of the loop if no known prefix is detected
        };
    }

    (current, modifiers)
}

fn parse_key_code_with_modifiers(
    raw: &str,
    mut modifiers: KeyModifiers,
) -> color_eyre::Result<KeyEvent, String> {
    let c = match raw {
        "esc" => KeyCode::Esc,
        "enter" => KeyCode::Enter,
        "left" => KeyCode::Left,
        "right" => KeyCode::Right,
        "up" => KeyCode::Up,
        "down" => KeyCode::Down,
        "home" => KeyCode::Home,
        "end" => KeyCode::End,
        "pageup" => KeyCode::PageUp,
        "pagedown" => KeyCode::PageDown,
        "backtab" => {
            modifiers.insert(KeyModifiers::SHIFT);
            KeyCode::BackTab
        }
        "backspace" => KeyCode::Backspace,
        "delete" => KeyCode::Delete,
        "insert" => KeyCode::Insert,
        "f1" => KeyCode::F(1),
        "f2" => KeyCode::F(2),
        "f3" => KeyCode::F(3),
        "f4" => KeyCode::F(4),
        "f5" => KeyCode::F(5),
        "f6" => KeyCode::F(6),
        "f7" => KeyCode::F(7),
        "f8" => KeyCode::F(8),
        "f9" => KeyCode::F(9),
        "f10" => KeyCode::F(10),
        "f11" => KeyCode::F(11),
        "f12" => KeyCode::F(12),
        "space" => KeyCode::Char(' '),
        "hyphen" => KeyCode::Char('-'),
        "minus" => KeyCode::Char('-'),
        "tab" => KeyCode::Tab,
        c if c.len() == 1 => {
            let mut c = c.chars().next().unwrap();
            if modifiers.contains(KeyModifiers::SHIFT) {
                c = c.to_ascii_uppercase();
            }
            KeyCode::Char(c)
        }
        _ => return Err(format!("Unable to parse {raw}")),
    };
    Ok(KeyEvent::new(c, modifiers))
}

pub fn key_event_to_string(key_event: &KeyEvent) -> String {
    let char;
    let key_code = match key_event.code {
        KeyCode::Backspace => "backspace",
        KeyCode::Enter => "enter",
        KeyCode::Left => "left",
        KeyCode::Right => "right",
        KeyCode::Up => "up",
        KeyCode::Down => "down",
        KeyCode::Home => "home",
        KeyCode::End => "end",
        KeyCode::PageUp => "pageup",
        KeyCode::PageDown => "pagedown",
        KeyCode::Tab => "tab",
        KeyCode::BackTab => "backtab",
        KeyCode::Delete => "delete",
        KeyCode::Insert => "insert",
        KeyCode::F(c) => {
            char = format!("f({c})");
            &char
        }
        KeyCode::Char(' ') => "space",
        KeyCode::Char(c) => {
            char = c.to_string();
            &char
        }
        KeyCode::Esc => "esc",
        KeyCode::Null => "",
        KeyCode::CapsLock => "",
        KeyCode::Menu => "",
        KeyCode::ScrollLock => "",
        KeyCode::Media(_) => "",
        KeyCode::NumLock => "",
        KeyCode::PrintScreen => "",
        KeyCode::Pause => "",
        KeyCode::KeypadBegin => "",
        KeyCode::Modifier(_) => "",
    };

    let mut modifiers = Vec::with_capacity(3);

    if key_event.modifiers.intersects(KeyModifiers::CONTROL) {
        modifiers.push("ctrl");
    }

    if key_event.modifiers.intersects(KeyModifiers::SHIFT) {
        modifiers.push("shift");
    }

    if key_event.modifiers.intersects(KeyModifiers::ALT) {
        modifiers.push("alt");
    }

    let mut key = modifiers.join("-");

    if !key.is_empty() {
        key.push('-');
    }
    key.push_str(key_code);

    key
}

pub fn parse_key_sequence(raw: &str) -> color_eyre::Result<Vec<KeyEvent>, String> {
    if raw.chars().filter(|c| *c == '>').count() != raw.chars().filter(|c| *c == '<').count() {
        return Err(format!("Unable to parse `{}`", raw));
    }
    let raw = if !raw.contains("><") {
        let raw = raw.strip_prefix('<').unwrap_or(raw);
        raw.strip_prefix('>').unwrap_or(raw)
    } else {
        raw
    };
    let sequences = raw
        .split("><")
        .map(|seq| {
            if let Some(s) = seq.strip_prefix('<') {
                s
            } else if let Some(s) = seq.strip_suffix('>') {
                s
            } else {
                seq
            }
        })
        .collect::<Vec<_>>();

    sequences.into_iter().map(parse_key_event).collect()
}

#[derive(Clone, Debug, Default)]
pub struct Styles(pub HashMap<Mode, HashMap<String, Style>>);

impl<'de> Deserialize<'de> for Styles {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let parsed_map = HashMap::<Mode, HashMap<String, String>>::deserialize(deserializer)?;

        let styles = parsed_map
            .into_iter()
            .map(|(mode, inner_map)| {
                let converted_inner_map = inner_map
                    .into_iter()
                    .map(|(str, style)| (str, parse_style(&style)))
                    .collect();
                (mode, converted_inner_map)
            })
            .collect();

        Ok(Styles(styles))
    }
}

pub fn parse_style(line: &str) -> Style {
    let (foreground, background) =
        line.split_at(line.to_lowercase().find("on ").unwrap_or(line.len()));
    let foreground = process_color_string(foreground);
    let background = process_color_string(&background.replace("on ", ""));

    let mut style = Style::default();
    if let Some(fg) = parse_color(&foreground.0) {
        style = style.fg(fg);
    }
    if let Some(bg) = parse_color(&background.0) {
        style = style.bg(bg);
    }
    style = style.add_modifier(foreground.1 | background.1);
    style
}

fn process_color_string(color_str: &str) -> (String, Modifier) {
    let color = color_str
        .replace("grey", "gray")
        .replace("bright ", "")
        .replace("bold ", "")
        .replace("underline ", "")
        .replace("inverse ", "");

    let mut modifiers = Modifier::empty();
    if color_str.contains("underline") {
        modifiers |= Modifier::UNDERLINED;
    }
    if color_str.contains("bold") {
        modifiers |= Modifier::BOLD;
    }
    if color_str.contains("inverse") {
        modifiers |= Modifier::REVERSED;
    }

    (color, modifiers)
}

fn parse_color(s: &str) -> Option<Color> {
    let s = s.trim_start();
    let s = s.trim_end();
    if s.contains("bright color") {
        let s = s.trim_start_matches("bright ");
        let c = s
            .trim_start_matches("color")
            .parse::<u8>()
            .unwrap_or_default();
        Some(Color::Indexed(c.wrapping_shl(8)))
    } else if s.contains("color") {
        let c = s
            .trim_start_matches("color")
            .parse::<u8>()
            .unwrap_or_default();
        Some(Color::Indexed(c))
    } else if s.contains("gray") {
        let c = 232
            + s.trim_start_matches("gray")
                .parse::<u8>()
                .unwrap_or_default();
        Some(Color::Indexed(c))
    } else if s.contains("rgb") {
        let red = (s.as_bytes()[3] as char).to_digit(10).unwrap_or_default() as u8;
        let green = (s.as_bytes()[4] as char).to_digit(10).unwrap_or_default() as u8;
        let blue = (s.as_bytes()[5] as char).to_digit(10).unwrap_or_default() as u8;
        let c = 16 + red * 36 + green * 6 + blue;
        Some(Color::Indexed(c))
    } else if s == "bold black" {
        Some(Color::Indexed(8))
    } else if s == "bold red" {
        Some(Color::Indexed(9))
    } else if s == "bold green" {
        Some(Color::Indexed(10))
    } else if s == "bold yellow" {
        Some(Color::Indexed(11))
    } else if s == "bold blue" {
        Some(Color::Indexed(12))
    } else if s == "bold magenta" {
        Some(Color::Indexed(13))
    } else if s == "bold cyan" {
        Some(Color::Indexed(14))
    } else if s == "bold white" {
        Some(Color::Indexed(15))
    } else if s == "black" {
        Some(Color::Indexed(0))
    } else if s == "red" {
        Some(Color::Indexed(1))
    } else if s == "green" {
        Some(Color::Indexed(2))
    } else if s == "yellow" {
        Some(Color::Indexed(3))
    } else if s == "blue" {
        Some(Color::Indexed(4))
    } else if s == "magenta" {
        Some(Color::Indexed(5))
    } else if s == "cyan" {
        Some(Color::Indexed(6))
    } else if s == "white" {
        Some(Color::Indexed(7))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use pretty_assertions::assert_eq;

    use super::*;

    #[test]
    fn test_parse_style_default() {
        let style = parse_style("");
        assert_eq!(style, Style::default());
    }

    #[test]
    fn dedup_dirs_keeps_first_occurrence_in_order() {
        let dirs = vec![
            PathBuf::from("/a"),
            PathBuf::from("/b"),
            PathBuf::from("/a"),
            PathBuf::from("/c"),
            PathBuf::from("/b"),
        ];
        assert_eq!(
            dedup_dirs(dirs),
            vec![
                PathBuf::from("/a"),
                PathBuf::from("/b"),
                PathBuf::from("/c"),
            ]
        );
    }

    #[test]
    fn config_search_dirs_includes_dot_config_bee_tui() {
        // ~/.config/bee-tui must be on the search path on every platform
        // so macOS / Windows devs don't need the platform-native dir.
        let dirs = config_search_dirs();
        assert!(
            dirs.iter().any(|d| d.ends_with(".config/bee-tui")),
            "expected ~/.config/bee-tui in search path, got {dirs:?}"
        );
    }

    #[test]
    fn test_parse_style_foreground() {
        let style = parse_style("red");
        assert_eq!(style.fg, Some(Color::Indexed(1)));
    }

    #[test]
    fn test_parse_style_background() {
        let style = parse_style("on blue");
        assert_eq!(style.bg, Some(Color::Indexed(4)));
    }

    #[test]
    fn test_parse_style_modifiers() {
        let style = parse_style("underline red on blue");
        assert_eq!(style.fg, Some(Color::Indexed(1)));
        assert_eq!(style.bg, Some(Color::Indexed(4)));
    }

    #[test]
    fn test_process_color_string() {
        let (color, modifiers) = process_color_string("underline bold inverse gray");
        assert_eq!(color, "gray");
        assert!(modifiers.contains(Modifier::UNDERLINED));
        assert!(modifiers.contains(Modifier::BOLD));
        assert!(modifiers.contains(Modifier::REVERSED));
    }

    #[test]
    fn test_parse_color_rgb() {
        let color = parse_color("rgb123");
        let expected = 16 + 36 + 2 * 6 + 3;
        assert_eq!(color, Some(Color::Indexed(expected)));
    }

    #[test]
    fn test_parse_color_unknown() {
        let color = parse_color("unknown");
        assert_eq!(color, None);
    }

    #[test]
    fn test_config() -> color_eyre::Result<()> {
        // Plain `q` is intercepted in App::handle_key_event for the
        // double-tap quit guard, so it is intentionally NOT in the
        // keybindings map. Ctrl+C remains as the immediate-quit
        // escape hatch.
        let c = Config::new()?;
        assert_eq!(
            c.keybindings
                .0
                .get(&Mode::Home)
                .unwrap()
                .get(&parse_key_sequence("<Ctrl-c>").unwrap_or_default())
                .unwrap(),
            &Action::Quit
        );
        Ok(())
    }

    #[test]
    fn test_simple_keys() {
        assert_eq!(
            parse_key_event("a").unwrap(),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::empty())
        );

        assert_eq!(
            parse_key_event("enter").unwrap(),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::empty())
        );

        assert_eq!(
            parse_key_event("esc").unwrap(),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::empty())
        );
    }

    #[test]
    fn test_with_modifiers() {
        assert_eq!(
            parse_key_event("ctrl-a").unwrap(),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        );

        assert_eq!(
            parse_key_event("alt-enter").unwrap(),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
        );

        assert_eq!(
            parse_key_event("shift-esc").unwrap(),
            KeyEvent::new(KeyCode::Esc, KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn test_multiple_modifiers() {
        assert_eq!(
            parse_key_event("ctrl-alt-a").unwrap(),
            KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )
        );

        assert_eq!(
            parse_key_event("ctrl-shift-enter").unwrap(),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::CONTROL | KeyModifiers::SHIFT)
        );
    }

    #[test]
    fn test_reverse_multiple_modifiers() {
        assert_eq!(
            key_event_to_string(&KeyEvent::new(
                KeyCode::Char('a'),
                KeyModifiers::CONTROL | KeyModifiers::ALT
            )),
            "ctrl-alt-a".to_string()
        );
    }

    #[test]
    fn test_invalid_keys() {
        assert!(parse_key_event("invalid-key").is_err());
        assert!(parse_key_event("ctrl-invalid-key").is_err());
    }

    #[test]
    fn test_case_insensitivity() {
        assert_eq!(
            parse_key_event("CTRL-a").unwrap(),
            KeyEvent::new(KeyCode::Char('a'), KeyModifiers::CONTROL)
        );

        assert_eq!(
            parse_key_event("AlT-eNtEr").unwrap(),
            KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)
        );
    }
}
