#![allow(dead_code)] // Remove this once you start using the code

use std::{collections::HashMap, env, path::PathBuf};

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

        let config_files = [
            ("config.json5", config::FileFormat::Json5),
            ("config.json", config::FileFormat::Json),
            ("config.yaml", config::FileFormat::Yaml),
            ("config.toml", config::FileFormat::Toml),
            ("config.ini", config::FileFormat::Ini),
        ];
        let mut found_config = false;
        for (file, format) in &config_files {
            let source = config::File::from(config_dir.join(file))
                .format(*format)
                .required(false);
            builder = builder.add_source(source);
            if config_dir.join(file).exists() {
                found_config = true
            }
        }
        if !found_config {
            error!("No configuration file found. Application may not behave as expected");
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

pub fn get_config_dir() -> PathBuf {
    if let Some(s) = CONFIG_FOLDER.clone() {
        s
    } else if let Some(proj_dirs) = project_directory() {
        proj_dirs.config_local_dir().to_path_buf()
    } else {
        PathBuf::from(".").join(".config")
    }
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
