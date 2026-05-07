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
        network::Network,
        peers::Peers,
        stamps::Stamps,
        swap::Swap,
        tags::Tags,
        warmup::Warmup,
    },
    config::Config,
    log_capture,
    state::State,
    theme,
    tui::{Event, Tui},
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
    "Health", "Stamps", "Swap", "Lottery", "Peers", "Network", "Warmup", "API", "Tags",
];

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
        let supervisor = match (bee_bin, bee_config) {
            (Some(bin), Some(cfg)) => {
                eprintln!("bee-tui: spawning bee {bin:?} --config {cfg:?}");
                let mut sup = BeeSupervisor::spawn(&bin, &cfg)?;
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
            command_status: None,
            help_visible: false,
            quit_pending: None,
            supervisor,
            bee_status: BeeStatus::Running,
            bee_log_rx,
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
            }
            KeyCode::Enter => {
                let line = std::mem::take(buf);
                self.command_buffer = None;
                self.execute_command(&line)?;
            }
            KeyCode::Backspace => {
                buf.pop();
            }
            KeyCode::Char(c) => {
                buf.push(c);
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
                self.command_status = Some(match self.export_diagnostic_bundle() {
                    Ok(path) => CommandStatus::Info(format!(
                        "diagnostic bundle exported to {}",
                        path.display()
                    )),
                    Err(e) => CommandStatus::Err(format!("diagnose failed: {e}")),
                });
            }
            "pins-check" | "pins" => {
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
                    "unknown command: {other:?} (try :health, :stamps, :swap, :lottery, :peers, :network, :warmup, :api, :tags, :diagnose, :pins-check, :loggers, :set-logger, :context, :quit)"
                )));
            }
        }
        Ok(())
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
        let gates = Health::gates_for(&health, Some(&topology));
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
        ("Shift+End", "resume auto-tail"),
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
        _ => &[],
    }
}

/// Construct the eight v0.3 screens with receivers from the supplied
/// hub. Extracted so `App::new` and the `:context` profile-switcher
/// can share the wiring — the screen list is the same on every
/// connection, only the underlying watch hub changes.
///
/// Order matters — the [`SCREEN_NAMES`] table assumes index 0 is
/// Health, 1 is Stamps, 2 is Swap, 3 is Lottery, 4 is Peers, 5 is
/// Network, 6 is Warmup, 7 is API, 8 is Tags.
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
    ]
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
}
