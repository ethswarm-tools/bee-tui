use std::path::PathBuf;
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
    bee_supervisor::{BeeStatus, BeeSupervisor},
    components::{
        Component,
        api_health::ApiHealth,
        health::{Gate, GateStatus, Health},
        log_pane::{BeeLogLine, LogPane, LogTab},
        lottery::Lottery,
        manifest::Manifest,
        network::Network,
        peers::Peers,
        pins::Pins,
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
    /// the cockpit isn't acting as the supervisor (no log file to
    /// tail). Drained on each Tick into the LogPane.
    bee_log_rx: Option<mpsc::UnboundedReceiver<(LogTab, BeeLogLine)>>,
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
    /// Per-gate transition tracker for the optional webhook alerter.
    /// On every Tick we feed it the latest gates; it returns the
    /// transitions worth pinging on (debounced per-gate). When
    /// `[alerts].webhook_url` is unset, [`Self::tick_alerts`] short-
    /// circuits before touching this state so the cost is one
    /// `Option::is_none` check per Tick.
    alert_state: crate::alerts::AlertState,
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
    ("manifest", "<ref> — open Mantaray tree browser at a reference"),
    ("inspect", "<ref> — what is this? auto-detects manifest vs raw chunk"),
    (
        "durability-check",
        "<ref> — walk chunk graph, report total / lost / errors",
    ),
    ("watchlist", "S13 Watchlist — durability-check history"),
    ("hash", "<path> — Swarm reference of a local file/dir (offline)"),
    ("cid", "<ref> [manifest|feed] — encode reference as CID"),
    ("depth-table", "Print canonical depth → capacity table"),
    ("gsoc-mine", "<overlay> <id> — mine a GSOC signer (CPU work)"),
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
}

/// Default timeout for waiting on `/health` after spawning Bee.
/// Bee's first start can include chain-state catch-up; a generous
/// budget here saves the operator from one false "didn't come up"
/// alarm. Override later via config if needed.
const BEE_API_READY_TIMEOUT: Duration = Duration::from_secs(60);

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
        let config = Config::new()?;
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
        let supervisor = match (bee_bin, bee_config) {
            (Some(bin), Some(cfg)) => {
                eprintln!("bee-tui: spawning bee {bin:?} --config {cfg:?}");
                let mut sup = BeeSupervisor::spawn(&bin, &cfg, bee_logs)?;
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
                Some(sup)
            }
            (Some(_), None) | (None, Some(_)) => {
                return Err(eyre!(
                    "[bee].bin and [bee].config must both be set (or both unset). \
                     Use --bee-bin AND --bee-config, or both fields in config.toml."
                ));
            }
            (None, None) => None,
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

        let screens = build_screens(&api, &watch);
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
        log_pane.set_spawn_active(supervisor.is_some());
        if let Some(c) = log_capture::cockpit_handle() {
            log_pane.set_cockpit_capture(c);
        }

        // Spawn the bee-log tailer if we own the supervisor. The
        // tailer parses each new line of the captured Bee log and
        // forwards `(LogTab, BeeLogLine)` pairs down an mpsc the
        // App drains every Tick. Inherits root_cancel so quit
        // unwinds it the same way as every other spawned task.
        let bee_log_rx = supervisor.as_ref().map(|sup| {
            let (tx, rx) = mpsc::unbounded_channel();
            crate::bee_log_tailer::spawn(
                sup.log_path().to_path_buf(),
                tx,
                root_cancel.child_token(),
            );
            rx
        });

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
            cmd_status_tx,
            cmd_status_rx,
            durability_tx,
            durability_rx,
            alert_state: crate::alerts::AlertState::new(config_alerts_debounce),
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
        let modal_before = self.command_buffer.is_some() || self.help_visible;
        match event {
            Event::Quit => action_tx.send(Action::Quit)?,
            Event::Tick => action_tx.send(Action::Tick)?,
            Event::Render => action_tx.send(Action::Render)?,
            Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
            Event::Key(key) => self.handle_key_event(key)?,
            _ => {}
        }
        let modal_after = self.command_buffer.is_some() || self.help_visible;
        // Non-key events (Tick / Resize / Render) always propagate
        // so screens keep refreshing under modals.
        let propagate = !((modal_before || modal_after) && matches!(event, Event::Key(_)));
        if propagate {
            for component in self.iter_components_mut() {
                if let Some(action) = component.handle_events(Some(event.clone()))? {
                    action_tx.send(action)?;
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
                _ => {}
            }
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
                let line = std::mem::take(buf);
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
                    "unknown command: {other:?} (try :health, :stamps, :swap, :lottery, :peers, :network, :warmup, :api, :tags, :pins, :manifest, :inspect, :diagnose, :pins-check, :loggers, :set-logger, :topup-preview, :dilute-preview, :extend-preview, :buy-preview, :buy-suggest, :plan-batch, :probe-upload, :hash, :cid, :depth-table, :gsoc-mine, :pss-target, :context, :quit)"
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
        CommandStatus::Info(format!("inspecting {label} — result will replace this line"))
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
        tokio::spawn(async move {
            let result = durability::check(api, reference).await;
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
        let path = match self
            .config
            .bee
            .as_ref()
            .map(|b| b.config.clone())
        {
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
        CommandStatus::Info(format!(
            "{} → {}",
            report.summary(),
            out_path.display()
        ))
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
            let running = api
                .bee()
                .debug()
                .health()
                .await
                .ok()
                .map(|h| h.version);
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
                    "usage: :plan-batch <batch-prefix> [usage-thr] [ttl-thr] [extra-depth]"
                        .into(),
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
        // Cancel the current hub's children and let it drop. The new
        // hub spawns under the same root_cancel so quit-time teardown
        // still walks the whole tree in one go.
        self.watch.shutdown();
        let refresh = RefreshProfile::from_config(&self.config.ui.refresh);
        let new_watch = BeeWatch::start_with_profile(new_api.clone(), &self.root_cancel, refresh);
        let new_health_rx = new_watch.health();
        let new_screens = build_screens(&new_api, &new_watch);
        self.api = new_api;
        self.watch = new_watch;
        self.health_rx = new_health_rx;
        self.screens = new_screens;
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
            let r = pprof_bundle::fetch_and_write(&base_url, auth_token, seconds, dir_for_task)
                .await;
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

    /// Per-Tick webhook alerter. No-op when `[alerts].webhook_url`
    /// is unset — operators get the cockpit's existing visual gates
    /// without any outbound traffic. When configured, we compute the
    /// same `Health::gates_for(...)` view the cockpit renders, diff
    /// against the previous Tick's status, and POST one webhook per
    /// transition that survives the per-gate debounce.
    fn tick_alerts(&mut self) {
        let url = match self.config.alerts.webhook_url.as_deref() {
            Some(u) if !u.is_empty() => u.to_string(),
            _ => return,
        };
        let health = self.health_rx.borrow().clone();
        let topology = self.watch.topology().borrow().clone();
        let stamps = self.watch.stamps().borrow().clone();
        let gates = Health::gates_for_with_stamps(&health, Some(&topology), Some(&stamps));
        let alerts = self.alert_state.diff_and_record(&gates);
        for alert in alerts {
            let url = url.clone();
            tokio::spawn(async move {
                if let Err(e) = crate::alerts::fire(&url, &alert).await {
                    tracing::warn!(target: "bee_tui::alerts", "webhook fire failed: {e}");
                }
            });
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
                        if let Some(idx) =
                            SCREEN_NAMES.iter().position(|n| *n == "Watchlist")
                        {
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
                    // Webhook health-gate alerts. Cheap when not
                    // configured (no clones, no work) — only computes
                    // gates and diffs when [alerts].webhook_url is set.
                    self.tick_alerts();
                }
                Action::Quit => self.should_quit = true,
                Action::Suspend => self.should_suspend = true,
                Action::Resume => self.should_suspend = false,
                Action::ClearScreen => tui.terminal.clear()?,
                Action::Resize(w, h) => self.handle_resize(tui, w, h)?,
                Action::Render => self.render(tui)?,
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
        let profile = self.api.name.clone();
        let endpoint = self.api.url.clone();
        let last_ping = self.health_rx.borrow().last_ping;
        let now_utc = format_utc_now();
        let bee_status_label = if self.supervisor.is_some() && !self.bee_status.is_running() {
            // Only show the status when (a) we're acting as the
            // supervisor and (b) something is wrong. Hiding the
            // happy-path label keeps the metadata line uncluttered.
            Some(self.bee_status.label())
        } else {
            None
        };
        tui.draw(|frame| {
            use ratatui::layout::{Constraint, Layout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span};
            use ratatui::widgets::Paragraph;

            let chunks = Layout::vertical([
                Constraint::Length(2),               // top-bar (metadata + tabs)
                Constraint::Min(0),                  // active screen
                Constraint::Length(1),               // command bar / status line
                Constraint::Length(log_pane_height), // tabbed log pane (operator-resizable)
            ])
            .split(frame.area());

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
            if let Some(label) = bee_status_label.as_ref() {
                metadata_spans.push(Span::raw("   "));
                metadata_spans.push(Span::styled(
                    format!(" {label} "),
                    Style::default()
                        .fg(Color::Black)
                        .bg(t.fail)
                        .add_modifier(Modifier::BOLD),
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

            // Active screen
            if let Some(screen) = screens.get_mut(active) {
                if let Err(err) = screen.draw(frame, chunks[1]) {
                    let _ = tx.send(Action::Error(format!("Failed to draw screen: {err:?}")));
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
                draw_help_overlay(frame, frame.area(), active, &theme);
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
    theme: &theme::Theme,
) {
    use ratatui::layout::Rect;
    use ratatui::style::{Modifier, Style};
    use ratatui::text::{Line, Span};
    use ratatui::widgets::{Block, Borders, Clear, Paragraph};

    let screen_name = SCREEN_NAMES.get(active_screen).copied().unwrap_or("?");
    let screen_rows = screen_keymap(active_screen);
    let global_rows: &[(&str, &str)] = &[
        ("Tab", "next screen"),
        ("Shift+Tab", "previous screen"),
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

    // Layout: pick the smaller of (screen size, 70x22) so we always
    // fit on small terminals.
    let w = area.width.min(72);
    let h = area.height.min(22);
    let x = area.x + (area.width.saturating_sub(w)) / 2;
    let y = area.y + (area.height.saturating_sub(h)) / 2;
    let rect = Rect {
        x,
        y,
        width: w,
        height: h,
    };

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
        "  Esc / ? / q to dismiss",
        Style::default()
            .fg(theme.dim)
            .add_modifier(Modifier::ITALIC),
    )));

    // `Clear` blanks the underlying rendered region so the overlay
    // doesn't ghost over screen content.
    frame.render_widget(Clear, rect);
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme.accent))
                .title(" help "),
        ),
        rect,
    );
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
fn build_screens(api: &Arc<ApiClient>, watch: &BeeWatch) -> Vec<Box<dyn Component>> {
    let health = Health::new(api.clone(), watch.health(), watch.topology());
    let stamps = Stamps::new(api.clone(), watch.stamps());
    let swap = Swap::new(watch.swap());
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
    ]
}

/// Build the 4104-byte (8 + 4096) synthetic chunk that
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
}
