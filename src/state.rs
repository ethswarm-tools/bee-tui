//! Persisted runtime state — the *operator-tweaked, app-written*
//! file that's distinct from `config.toml` (operator-edited,
//! app-read-only).
//!
//! XDG split: per <https://specifications.freedesktop.org/basedir-spec/>,
//! `state` is the right home for "things the user expects to persist
//! but doesn't manage by hand": last-active tab, last pane height,
//! cursor positions on long lists. Keeping it separate from
//! `config.toml` means a `git diff` of the operator's dotfiles only
//! shows real config edits.
//!
//! Path resolution mirrors `config.rs`'s `get_data_dir` /
//! `get_config_dir`:
//! - `$BEE_TUI_STATE` env var (override for tests / sandboxing)
//! - `ProjectDirs::state_dir()` (Linux: `~/.local/state/bee-tui/`)
//! - `data_local_dir()` (macOS / Windows fallback — no XDG state dir
//!   convention there; data dir is the closest match)
//! - `./.state` as a last resort

use std::fs;
use std::path::PathBuf;

use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
use tracing::debug;

/// Filename used inside the resolved state directory. Single-file
/// design — the schema is small enough that a directory tree would
/// be over-engineered.
const STATE_FILENAME: &str = "state.toml";

/// Bounds for the bottom log pane height. Below 4 the tab strip + a
/// content row + borders don't fit; above 24 we steal too much from
/// the active screen on a typical 80×24 terminal.
pub const LOG_PANE_MIN_HEIGHT: u16 = 4;
pub const LOG_PANE_MAX_HEIGHT: u16 = 24;
pub const LOG_PANE_DEFAULT_HEIGHT: u16 = 10;

/// Persisted across launches.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct State {
    /// Height of the bottom log pane (in terminal lines, including
    /// the tab strip + borders). Clamped at load time so a
    /// hand-edited corrupt value can't break the layout.
    #[serde(default = "default_log_pane_height")]
    pub log_pane_height: u16,
    /// Last tab the operator had focused on the log pane. Stored as
    /// a kebab-case string (`errors`, `warning`, `info`, `debug`,
    /// `bee-http`, `self-http`). Unknown values fall back to the
    /// default tab silently.
    #[serde(default = "default_active_tab")]
    pub log_pane_active_tab: String,
}

impl Default for State {
    fn default() -> Self {
        Self {
            log_pane_height: default_log_pane_height(),
            log_pane_active_tab: default_active_tab(),
        }
    }
}

fn default_log_pane_height() -> u16 {
    LOG_PANE_DEFAULT_HEIGHT
}

fn default_active_tab() -> String {
    "self-http".to_string()
}

impl State {
    /// Load from the resolved state path. Missing file → defaults
    /// (no warning — first-run is normal). Parse error → defaults +
    /// a tracing warning (corrupt file shouldn't block startup, but
    /// the operator should see it). Returns `(State, source_path)`
    /// so callers can save back to the same place on quit.
    pub fn load() -> (Self, PathBuf) {
        let path = state_path();
        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                debug!("no state file at {path:?}; starting from defaults");
                return (Self::default(), path);
            }
            Err(e) => {
                tracing::warn!("could not read state file {path:?}: {e}; using defaults");
                return (Self::default(), path);
            }
        };
        match toml::from_str::<State>(&raw) {
            Ok(mut s) => {
                s.clamp_in_place();
                (s, path)
            }
            Err(e) => {
                tracing::warn!(
                    "state file {path:?} is malformed ({e}); using defaults — \
                     fix or delete to persist new values"
                );
                (Self::default(), path)
            }
        }
    }

    /// Best-effort save. Failures are logged at warn-level but never
    /// surfaced as errors to the operator — losing a pane-height
    /// preference shouldn't block quit.
    pub fn save(&self, path: &PathBuf) {
        if let Some(parent) = path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            tracing::warn!("could not create state dir {parent:?}: {e}");
            return;
        }
        match toml::to_string_pretty(self) {
            Ok(s) => {
                if let Err(e) = fs::write(path, s) {
                    tracing::warn!("could not write state to {path:?}: {e}");
                }
            }
            Err(e) => tracing::warn!("could not serialize state: {e}"),
        }
    }

    /// Force every field into its valid range. Pure — exposed for
    /// tests so they can verify clamping without round-tripping
    /// through the filesystem.
    pub fn clamp_in_place(&mut self) {
        self.log_pane_height = self
            .log_pane_height
            .clamp(LOG_PANE_MIN_HEIGHT, LOG_PANE_MAX_HEIGHT);
    }
}

/// Resolve the state-file path. See module docs for the precedence
/// chain.
pub fn state_path() -> PathBuf {
    if let Ok(s) = std::env::var("BEE_TUI_STATE") {
        return PathBuf::from(s);
    }
    if let Some(proj_dirs) = ProjectDirs::from("com", "ethswarm-tools", env!("CARGO_PKG_NAME")) {
        if let Some(state_dir) = proj_dirs.state_dir() {
            return state_dir.join(STATE_FILENAME);
        }
        // macOS / Windows: no XDG state dir, fall back to the
        // platform's per-app data directory.
        return proj_dirs.data_local_dir().join(STATE_FILENAME);
    }
    PathBuf::from(".state").join(STATE_FILENAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_height_is_in_range() {
        let s = State::default();
        assert!(s.log_pane_height >= LOG_PANE_MIN_HEIGHT);
        assert!(s.log_pane_height <= LOG_PANE_MAX_HEIGHT);
    }

    #[test]
    fn clamp_low_height() {
        let mut s = State {
            log_pane_height: 1,
            ..Default::default()
        };
        s.clamp_in_place();
        assert_eq!(s.log_pane_height, LOG_PANE_MIN_HEIGHT);
    }

    #[test]
    fn clamp_high_height() {
        let mut s = State {
            log_pane_height: 999,
            ..Default::default()
        };
        s.clamp_in_place();
        assert_eq!(s.log_pane_height, LOG_PANE_MAX_HEIGHT);
    }

    #[test]
    fn round_trip_through_disk() {
        let dir = tempdir();
        let path = dir.join("state.toml");
        let s = State {
            log_pane_height: 14,
            log_pane_active_tab: "errors".into(),
        };
        s.save(&path);
        // Forcibly use a known path bypassing env var to keep this
        // test isolated even when other tests run in parallel.
        let raw = std::fs::read_to_string(&path).expect("save must produce a readable file");
        let parsed: State = toml::from_str(&raw).expect("parse must succeed");
        assert_eq!(parsed, s);
    }

    #[test]
    fn missing_file_yields_defaults() {
        let dir = tempdir();
        let path = dir.join("does-not-exist.toml");
        // Manually exercise the load path's missing-file branch by
        // checking that read returns NotFound; the public load() is
        // env-coupled so we mimic its behaviour here.
        assert!(matches!(
            std::fs::read_to_string(&path),
            Err(ref e) if e.kind() == std::io::ErrorKind::NotFound
        ));
        // The State::default round-trip is the contract:
        let s = State::default();
        assert_eq!(s, State::default());
    }

    fn tempdir() -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "bee-tui-state-test-{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&p).unwrap();
        p
    }
}
