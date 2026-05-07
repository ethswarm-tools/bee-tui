use std::sync::Arc;

use color_eyre::eyre::eyre;
use crossterm::event::KeyEvent;
use ratatui::prelude::Rect;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, info};

use crate::{
    action::Action,
    api::ApiClient,
    components::{
        Component, command_log::CommandLog, health::Health, lottery::Lottery, peers::Peers,
        stamps::Stamps, swap::Swap,
    },
    config::Config,
    log_capture,
    tui::{Event, Tui},
    watch::BeeWatch,
};

pub struct App {
    config: Config,
    tick_rate: f64,
    frame_rate: f64,
    /// Top-level screens, in display order. Tab cycles among them
    /// (v0.4 will replace this with a k9s-style `:command` resource
    /// switcher).
    screens: Vec<Box<dyn Component>>,
    /// Index into [`Self::screens`] for the currently visible screen.
    current_screen: usize,
    /// Always-on bottom strip; not part of `screens` because it
    /// renders alongside whatever screen is active.
    command_log: Box<dyn Component>,
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
}

/// Names the top-level screens. Index matches position in
/// [`App::screens`].
const SCREEN_NAMES: &[&str] = &["Health", "Stamps", "Swap", "Lottery", "Peers"];

#[derive(Default, Debug, Copy, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Mode {
    #[default]
    Home,
}

impl App {
    pub fn new(tick_rate: f64, frame_rate: f64) -> color_eyre::Result<Self> {
        let (action_tx, action_rx) = mpsc::unbounded_channel();
        let config = Config::new()?;

        // Pick the active node profile and build an ApiClient for it.
        let node = config
            .active_node()
            .ok_or_else(|| eyre!("no Bee node configured (config.nodes is empty)"))?;
        let api = Arc::new(ApiClient::from_node(node)?);

        // Spawn the watch / informer hub. Pollers attach to children
        // of `root_cancel`, so quitting cancels everything in one go.
        let root_cancel = CancellationToken::new();
        let watch = BeeWatch::start(api.clone(), &root_cancel);

        // S1 Health is the default screen for v0.1. v0.3 onwards it
        // also subscribes to the topology stream so the
        // bin-saturation gate can show real per-bin starvation
        // status instead of a placeholder.
        let health = Health::new(api.clone(), watch.health(), watch.topology());
        // S2 Stamps is the second screen — Tab to switch.
        let stamps = Stamps::new(watch.stamps());
        // S3 SWAP / cheques — third tab.
        let swap = Swap::new(watch.swap());
        // S4 Lottery — fourth tab. Subscribes to both the 2 s
        // redistribution-state stream (off the health hub) and the
        // 30 s /stake stream. Owns its own ApiClient handle so the
        // on-demand rchash benchmark ('r' key) doesn't have to round-
        // trip through the App-level action pipeline.
        let lottery = Lottery::new(api.clone(), watch.health(), watch.lottery());
        // S6 Peers + bin saturation — fifth tab. Driven by /topology
        // at 5 s; the bin-saturation classification depends on the
        // full per-bin BinInfo populated by bee-rs 1.4.
        let peers = Peers::new(watch.topology());
        // S10 Command-log subscribes to the bee::http capture set up
        // by logging::init. If logging hasn't initialised the capture
        // (e.g. running in a test harness), the pane just shows
        // "waiting for first request…".
        let command_log: Box<dyn Component> = Box::new(CommandLog::new(log_capture::handle()));

        Ok(Self {
            tick_rate,
            frame_rate,
            // Order matters — the SCREEN_NAMES table assumes index 0
            // is Health, index 1 is Stamps, index 2 is Swap, index 3
            // is Lottery, index 4 is Peers.
            screens: vec![
                Box::new(health),
                Box::new(stamps),
                Box::new(swap),
                Box::new(lottery),
                Box::new(peers),
            ],
            current_screen: 0,
            command_log,
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
        tui.exit()?;
        Ok(())
    }

    async fn handle_events(&mut self, tui: &mut Tui) -> color_eyre::Result<()> {
        let Some(event) = tui.next_event().await else {
            return Ok(());
        };
        let action_tx = self.action_tx.clone();
        match event {
            Event::Quit => action_tx.send(Action::Quit)?,
            Event::Tick => action_tx.send(Action::Tick)?,
            Event::Render => action_tx.send(Action::Render)?,
            Event::Resize(x, y) => action_tx.send(Action::Resize(x, y))?,
            Event::Key(key) => self.handle_key_event(key)?,
            _ => {}
        }
        for component in self.iter_components_mut() {
            if let Some(action) = component.handle_events(Some(event.clone()))? {
                action_tx.send(action)?;
            }
        }
        Ok(())
    }

    /// Iterate every component (screens + command-log strip) for
    /// uniform lifecycle ticks. Doesn't conflict with rendering,
    /// which only draws the active screen.
    fn iter_components_mut(&mut self) -> impl Iterator<Item = &mut Box<dyn Component>> {
        self.screens
            .iter_mut()
            .chain(std::iter::once(&mut self.command_log))
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> color_eyre::Result<()> {
        let action_tx = self.action_tx.clone();
        // Hard-coded screen-switch hotkey for v0.1; v0.2 routes this
        // through the regular keybinding table once the `:command`
        // bar lands.
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

    fn handle_actions(&mut self, tui: &mut Tui) -> color_eyre::Result<()> {
        while let Ok(action) = self.action_rx.try_recv() {
            if action != Action::Tick && action != Action::Render {
                debug!("{action:?}");
            }
            match action {
                Action::Tick => {
                    self.last_tick_key_events.drain(..);
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
        let command_log = &mut self.command_log;
        tui.draw(|frame| {
            use ratatui::layout::{Constraint, Layout};
            use ratatui::style::{Color, Modifier, Style};
            use ratatui::text::{Line, Span};
            use ratatui::widgets::Paragraph;

            let chunks = Layout::vertical([
                Constraint::Length(1), // top-bar tabs
                Constraint::Min(0),    // active screen
                Constraint::Length(8), // command-log strip
            ])
            .split(frame.area());

            // Top-bar: tab strip with the active screen highlighted.
            let mut tabs = Vec::with_capacity(SCREEN_NAMES.len() * 2);
            for (i, name) in SCREEN_NAMES.iter().enumerate() {
                let style = if i == active {
                    Style::default()
                        .fg(Color::Black)
                        .bg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                tabs.push(Span::styled(format!(" {name} "), style));
                tabs.push(Span::raw(" "));
            }
            tabs.push(Span::styled(
                "Tab to switch",
                Style::default().fg(Color::DarkGray),
            ));
            frame.render_widget(Paragraph::new(Line::from(tabs)), chunks[0]);

            // Active screen
            if let Some(screen) = screens.get_mut(active) {
                if let Err(err) = screen.draw(frame, chunks[1]) {
                    let _ = tx.send(Action::Error(format!("Failed to draw screen: {err:?}")));
                }
            }
            // Command-log strip
            if let Err(err) = command_log.draw(frame, chunks[2]) {
                let _ = tx.send(Action::Error(format!("Failed to draw log: {err:?}")));
            }
        })?;
        Ok(())
    }
}
