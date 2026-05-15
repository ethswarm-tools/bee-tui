#![allow(dead_code)] // Remove this once you start using the code

use std::{
    collections::{HashMap, HashSet},
    env,
    path::{Path, PathBuf},
};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use directories::ProjectDirs;
use lazy_static::lazy_static;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, de::Deserializer};
use tracing::error;

use crate::{action::Action, app::Mode};

const CONFIG: &str = include_str!("../.config/config.json5");

// All the data-schema items live in `bee_cockpit_core::config` —
// re-exported here so existing `crate::config::*` paths inside bee-tui
// keep working. What stays in this file (below): the `Config` struct
// itself (because it references the renderer-only `KeyBindings` and
// `Styles` types), KeyBindings + Styles + their parsers, and the
// file-discovery + `Config::load` machinery.
pub use bee_cockpit_core::config::{
    AlertsConfig, AppConfig, BeeConfig, BeeLogsConfig, BeeSupervisorConfig, DurabilityConfig,
    EconomicsConfig, FleetConfig, MetricsConfig, NodeConfig, NotificationsConfig, PubsubConfig,
    UiConfig,
};

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

/// Prepend `http://` to a scheme-less URL so `localhost:1633` works
/// as a positional argument.
fn normalize_url(url: &str) -> String {
    if url.starts_with("http://") || url.starts_with("https://") {
        url.to_string()
    } else {
        format!("http://{url}")
    }
}

/// Extract the host (no scheme, no port, no path) from a URL.
/// Handles `[ipv6]:port` and `host:port` forms.
fn host_of(url: &str) -> &str {
    let no_scheme = url.split_once("://").map(|(_, r)| r).unwrap_or(url);
    let host_port = no_scheme.split(['/', '?', '#']).next().unwrap_or(no_scheme);
    if let Some(rest) = host_port.strip_prefix('[') {
        // `[ipv6]:port` → `ipv6`
        rest.split(']').next().unwrap_or(rest)
    } else {
        // `host:port` → `host`, but only when the suffix is a port.
        match host_port.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) => h,
            _ => host_port,
        }
    }
}

/// Derive a short node name from a URL. A multi-label domain
/// (`bee-eu.example.com`) collapses to its first label (`bee-eu`);
/// bare hosts (`localhost`) and IP literals are kept whole. Returns
/// an empty string when no host can be parsed.
fn node_name_from_url(url: &str) -> String {
    let host = host_of(url);
    if host.is_empty() {
        return String::new();
    }
    let ip_like = host.chars().all(|c| c.is_ascii_digit() || c == '.');
    if !ip_like && host.contains('.') {
        host.split('.').next().unwrap_or(host).to_string()
    } else {
        host.to_string()
    }
}

/// Build an ad-hoc node list from positional URL arguments
/// (`bee-tui url1 url2 …`). The first URL is the default/active
/// node; names are derived from each URL's host with a `-2`, `-3`,
/// … suffix on collision and a `nodeN` fallback when no host parses.
/// Scheme-less URLs are normalised to `http://`.
pub fn nodes_from_urls(urls: &[String]) -> Vec<NodeConfig> {
    let mut used: HashSet<String> = HashSet::new();
    urls.iter()
        .enumerate()
        .map(|(i, raw)| {
            let url = normalize_url(raw);
            let derived = node_name_from_url(&url);
            let base = if derived.is_empty() {
                format!("node{}", i + 1)
            } else {
                derived
            };
            let mut name = base.clone();
            let mut n = 2;
            while !used.insert(name.clone()) {
                name = format!("{base}-{n}");
                n += 1;
            }
            NodeConfig {
                name,
                url,
                token: None,
                log_file: None,
                log_command: None,
                default: i == 0,
            }
        })
        .collect()
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
    /// Load config from the standard directory search path
    /// (see [`config_search_dirs`]).
    pub fn new() -> color_eyre::Result<Self, config::ConfigError> {
        Self::load(None)
    }

    /// Load config. When `explicit_file` is `Some`, that exact file is
    /// loaded — it must exist and have a recognised extension, and the
    /// directory search is skipped entirely. This backs the `--config`
    /// CLI flag. When `None`, the standard search path is used.
    pub fn load(explicit_file: Option<&Path>) -> color_eyre::Result<Self, config::ConfigError> {
        let default_config: Config = json5::from_str(CONFIG).unwrap();
        let data_dir = get_data_dir();
        let config_dir = get_config_dir();
        let mut builder = config::Config::builder()
            .set_default("data_dir", data_dir.to_str().unwrap())?
            .set_default("config_dir", config_dir.to_str().unwrap())?;

        if let Some(file) = explicit_file {
            let format = format_from_extension(file).ok_or_else(|| {
                config::ConfigError::Message(format!(
                    "unrecognised config file extension for {} — expected one of: \
                     toml, json5, json, yaml, yml, ini",
                    file.display()
                ))
            })?;
            if !file.exists() {
                return Err(config::ConfigError::Message(format!(
                    "config file not found: {}",
                    file.display()
                )));
            }
            builder = builder.add_source(
                config::File::from(file.to_path_buf())
                    .format(format)
                    .required(true),
            );
        } else {
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
                    "No configuration file found. Searched: {}. \
                     Application may not behave as expected",
                    search_dirs
                        .iter()
                        .map(|d| d.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                );
            }
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

/// Map a config file path to its [`config::FileFormat`] by extension.
/// Returns `None` for an unrecognised or missing extension. Backs the
/// `--config <file>` flag, which (unlike the directory search) has no
/// fixed file name to key the format off.
fn format_from_extension(path: &Path) -> Option<config::FileFormat> {
    match path
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase()
        .as_str()
    {
        "toml" => Some(config::FileFormat::Toml),
        "json5" => Some(config::FileFormat::Json5),
        "json" => Some(config::FileFormat::Json),
        "yaml" | "yml" => Some(config::FileFormat::Yaml),
        "ini" => Some(config::FileFormat::Ini),
        _ => None,
    }
}

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
    fn format_from_extension_maps_known_extensions() {
        use config::FileFormat;
        assert_eq!(
            format_from_extension(Path::new("a/b/config.toml")),
            Some(FileFormat::Toml)
        );
        assert_eq!(
            format_from_extension(Path::new("nodes.JSON5")),
            Some(FileFormat::Json5)
        );
        assert_eq!(
            format_from_extension(Path::new("nodes.yml")),
            Some(FileFormat::Yaml)
        );
        assert_eq!(
            format_from_extension(Path::new("nodes.yaml")),
            Some(FileFormat::Yaml)
        );
        assert_eq!(
            format_from_extension(Path::new("nodes.ini")),
            Some(FileFormat::Ini)
        );
        assert_eq!(format_from_extension(Path::new("nodes.conf")), None);
        assert_eq!(format_from_extension(Path::new("nodes")), None);
    }

    #[test]
    fn node_name_from_url_derives_short_names() {
        assert_eq!(node_name_from_url("http://localhost:1633"), "localhost");
        assert_eq!(
            node_name_from_url("https://bee-eu.example.com:1633"),
            "bee-eu"
        );
        // IP literals are kept whole, not collapsed at the first dot.
        assert_eq!(node_name_from_url("http://10.0.1.5:1633"), "10.0.1.5");
        // IPv6 in brackets.
        assert_eq!(node_name_from_url("http://[::1]:1633"), "::1");
        // Scheme-less + path are tolerated.
        assert_eq!(node_name_from_url("bee.example.org/"), "bee");
    }

    #[test]
    fn nodes_from_urls_builds_adhoc_fleet() {
        let nodes = nodes_from_urls(&[
            "http://localhost:1633".to_string(),
            "bee-eu.example.com:1633".to_string(), // scheme-less
        ]);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].name, "localhost");
        assert_eq!(nodes[0].url, "http://localhost:1633");
        assert!(nodes[0].default);
        // Scheme-less arg was normalised to http://.
        assert_eq!(nodes[1].name, "bee-eu");
        assert_eq!(nodes[1].url, "http://bee-eu.example.com:1633");
        assert!(!nodes[1].default);
    }

    #[test]
    fn nodes_from_urls_disambiguates_colliding_names() {
        let nodes = nodes_from_urls(&[
            "http://bee.a.com:1633".to_string(),
            "http://bee.b.com:1633".to_string(),
            "http://bee.c.com:1633".to_string(),
        ]);
        // All three collapse to "bee" — suffix the dupes.
        assert_eq!(nodes[0].name, "bee");
        assert_eq!(nodes[1].name, "bee-2");
        assert_eq!(nodes[2].name, "bee-3");
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
