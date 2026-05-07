//! Centralized colour palette. Set once at startup from
//! [`crate::config::UiConfig::theme`]; read by screens via
//! [`active`].
//!
//! Themes are deliberately *slot-based*: each slot is a semantic
//! intent (Pass / Warn / Fail / Header / etc.), not a literal colour.
//! Components ask the active theme "what colour for Warn?" rather
//! than hard-coding `Color::Yellow` — that's what makes a `mono`
//! variant possible without rewriting every component.
//!
//! The migration is incremental: components flip from hard-coded
//! [`Color`] literals to [`active().pass`] etc. as their commits
//! land. The first migrated component (S1 Health) ships in the same
//! commit as the theme module.

use std::sync::OnceLock;

use ratatui::style::Color;

use crate::config::UiConfig;

/// Slot-based colour palette. New slots get added as components are
/// migrated; never break a slot's *meaning* between releases.
#[derive(Debug, Clone, Copy)]
pub struct Theme {
    /// Bold / accent text — section titles, badges.
    pub accent: Color,
    /// Healthy / Pass status.
    pub pass: Color,
    /// Cautionary / Warn / InProgress status.
    pub warn: Color,
    /// Failure / error / Fail status.
    pub fail: Color,
    /// Informational / cyan (ping value, hashes).
    pub info: Color,
    /// Quiet / dim — labels, footnotes, "—" placeholders.
    pub dim: Color,
    /// Background highlight for the active tab strip.
    pub tab_active_bg: Color,
    /// Foreground for the active tab strip.
    pub tab_active_fg: Color,
}

impl Theme {
    /// Vibrant default: green/yellow/red status, cyan accents.
    pub const fn default_palette() -> Self {
        Self {
            accent: Color::Yellow,
            pass: Color::Green,
            warn: Color::Yellow,
            fail: Color::Red,
            info: Color::Cyan,
            dim: Color::DarkGray,
            tab_active_bg: Color::Yellow,
            tab_active_fg: Color::Black,
        }
    }

    /// Monochrome variant for terminals where colour is muted or
    /// distracting. Status is encoded by *intensity* + the existing
    /// glyphs (`✓ ⚠ ✗`) instead of hue.
    pub const fn mono() -> Self {
        Self {
            accent: Color::White,
            pass: Color::White,
            warn: Color::Gray,
            fail: Color::White,
            info: Color::Gray,
            dim: Color::DarkGray,
            tab_active_bg: Color::White,
            tab_active_fg: Color::Black,
        }
    }
}

impl Default for Theme {
    fn default() -> Self {
        Self::default_palette()
    }
}

static ACTIVE: OnceLock<Theme> = OnceLock::new();

/// Resolve a theme name to a [`Theme`]. Unknown names fall back to
/// the default palette with a single tracing warning so the operator
/// notices the typo without the binary refusing to start.
pub fn from_name(name: &str) -> Theme {
    match name {
        "default" => Theme::default_palette(),
        "mono" => Theme::mono(),
        other => {
            tracing::warn!(theme = %other, "unknown theme name; falling back to default");
            Theme::default_palette()
        }
    }
}

/// Set the active theme from the `[ui]` config section. Called once
/// during [`crate::app::App::new`]. Re-calling silently no-ops; full
/// runtime theme switching is a v0.6 feature.
pub fn install(ui: &UiConfig) {
    let _ = ACTIVE.set(from_name(&ui.theme));
}

/// Read the active theme. Returns the default palette if [`install`]
/// was never called (e.g. in unit tests that don't go through
/// `App::new`).
pub fn active() -> &'static Theme {
    static FALLBACK: Theme = Theme::default_palette();
    ACTIVE.get().unwrap_or(&FALLBACK)
}

/// Classify a header-line error message into a render colour + a
/// friendlier prefix. Bee returns
/// `HTTP 503: Node is syncing. This endpoint is unavailable. Try
/// again later.` for almost every endpoint during the first few
/// minutes after startup; rendering that in red as a hard error
/// gives a terrible first impression. We detect the syncing case
/// and render it in `warn` colour with a one-sentence explanation.
///
/// Returned tuple: `(colour, formatted message ready to render)`.
pub fn classify_header_error(err: &str) -> (Color, String) {
    if err.to_lowercase().contains("syncing") {
        (
            active().warn,
            "syncing — Bee is still bootstrapping; this view will populate once it catches up"
                .into(),
        )
    } else {
        (active().fail, format!("error: {err}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn from_name_recognises_known() {
        assert_eq!(
            std::mem::discriminant(&from_name("default").pass),
            std::mem::discriminant(&Color::Green)
        );
        assert_eq!(from_name("mono").pass, Color::White);
    }

    #[test]
    fn from_name_falls_back_on_unknown() {
        // Same shape as default — no panic, just a warn log.
        let t = from_name("not-a-real-theme");
        assert_eq!(t.pass, Theme::default_palette().pass);
    }

    #[test]
    fn active_returns_fallback_when_not_installed() {
        // OnceLock is process-global — under cargo test it may be
        // installed by another test that ran first. Either branch is
        // valid; just check we get *some* theme without panicking.
        let _ = active();
    }
}
