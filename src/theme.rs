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

/// Slot-based glyph set, sibling of [`Theme`]. Components ask
/// `theme::active().glyphs.pass` for the "Pass" glyph rather than
/// hardcoding `"✓"` — that's what makes `--ascii` work without
/// touching every screen.
///
/// Both variants are intentionally short (1–4 chars) so column
/// alignment in tables doesn't break when an operator switches
/// modes mid-session.
#[derive(Debug, Clone, Copy)]
pub struct Glyphs {
    /// Pass / healthy / synced.
    pub pass: &'static str,
    /// Warn / cautionary / skewed.
    pub warn: &'static str,
    /// Fail / critical / expired.
    pub fail: &'static str,
    /// Pending — chain-confirmation gating, queued.
    pub pending: &'static str,
    /// In-progress / partial — the bee::warmup sub-step glyph.
    pub in_progress: &'static str,
    /// Filled bar segment in fill bars.
    pub bar_filled: &'static str,
    /// Empty bar segment in fill bars.
    pub bar_empty: &'static str,
    /// Selection cursor on selectable rows (S2 stamps, S6 peers).
    pub cursor: &'static str,
    /// Truncation suffix for short addresses (`abc…123`).
    pub ellipsis: &'static str,
    /// Tree-continuation glyph for tooltip lines under a row.
    pub continuation: &'static str,
    /// Bullet / separator in dense one-liners (`prod-1 · 12ms · …`).
    pub bullet: &'static str,
    /// Em dash placeholder for missing values (`—`).
    pub em_dash: &'static str,
}

impl Glyphs {
    /// The default Unicode set. Looks great in modern terminals
    /// (kitty, WezTerm, iTerm2, recent gnome-terminal).
    pub const fn unicode() -> Self {
        Self {
            pass: "✓",
            warn: "⚠",
            fail: "✗",
            pending: "⏳",
            in_progress: "▒",
            bar_filled: "▇",
            bar_empty: "░",
            cursor: "▶",
            ellipsis: "…",
            continuation: "└─",
            bullet: "·",
            em_dash: "—",
        }
    }

    /// ASCII-only fallback for terminals without good Unicode
    /// support (Windows Terminal pre-Win11, some SSH chains, screen
    /// readers). All entries stay 1–4 chars so column alignment is
    /// stable.
    pub const fn ascii() -> Self {
        Self {
            pass: "OK",
            warn: "!",
            fail: "X",
            pending: "..",
            in_progress: "##",
            bar_filled: "#",
            bar_empty: ".",
            cursor: ">",
            ellipsis: "...",
            continuation: "+-",
            bullet: "|",
            em_dash: "--",
        }
    }
}

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
    /// Active glyph set. Driven by `--ascii` / `[ui].ascii_fallback`.
    pub glyphs: Glyphs,
}

impl Theme {
    /// Vibrant default: green/yellow/red status, cyan accents,
    /// Unicode glyphs.
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
            glyphs: Glyphs::unicode(),
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
            glyphs: Glyphs::unicode(),
        }
    }

    /// Override the glyph set in-place. Used by [`install_with_overrides`]
    /// when `--ascii` / `NO_COLOR` resolution decides Unicode is not
    /// safe.
    pub const fn with_glyphs(mut self, glyphs: Glyphs) -> Self {
        self.glyphs = glyphs;
        self
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

/// Resolution rules for `--ascii` / `--no-color` / `NO_COLOR`,
/// applied on top of the `[ui]` config section. CLI flags win over
/// env, env wins over config — the operator who typed `--ascii`
/// gets ASCII, and a `NO_COLOR=1` runtime override on a coloured
/// `[ui].theme = "default"` config still demotes to mono.
///
/// `force_no_color` should be `true` if either the `--no-color`
/// flag is set OR a non-empty `NO_COLOR` environment variable is
/// present (per the [no-color.org](https://no-color.org) spec).
/// `force_ascii` should be `true` for `--ascii`; the
/// `[ui].ascii_fallback` boolean from config is OR'd in here too.
///
/// Re-calling silently no-ops, same as [`install`].
pub fn install_with_overrides(ui: &UiConfig, force_no_color: bool, force_ascii: bool) {
    let palette_name = if force_no_color {
        "mono"
    } else {
        ui.theme.as_str()
    };
    let ascii = force_ascii || ui.ascii_fallback;
    let glyphs = if ascii {
        Glyphs::ascii()
    } else {
        Glyphs::unicode()
    };
    let theme = from_name(palette_name).with_glyphs(glyphs);
    let _ = ACTIVE.set(theme);
}

/// Detect whether the host terminal asked us to suppress colour.
/// Honours the [no-color.org](https://no-color.org) convention —
/// any non-empty `NO_COLOR` env var means "no colour".
pub fn no_color_env() -> bool {
    std::env::var("NO_COLOR")
        .map(|v| !v.is_empty())
        .unwrap_or(false)
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

    #[test]
    fn glyph_sets_are_distinct_and_short() {
        let u = Glyphs::unicode();
        let a = Glyphs::ascii();
        assert_ne!(u.pass, a.pass);
        assert_ne!(u.fail, a.fail);
        // Every ascii glyph stays at most 4 chars so column widths
        // don't drift between modes.
        for g in [
            a.pass,
            a.warn,
            a.fail,
            a.pending,
            a.in_progress,
            a.bar_filled,
            a.bar_empty,
            a.cursor,
            a.ellipsis,
            a.continuation,
            a.bullet,
            a.em_dash,
        ] {
            assert!(g.len() <= 4, "ascii glyph too wide: {g:?}");
        }
    }

    #[test]
    fn theme_with_glyphs_swaps_glyphs_only() {
        let t = Theme::default_palette().with_glyphs(Glyphs::ascii());
        assert_eq!(t.pass, Color::Green); // palette unchanged
        assert_eq!(t.glyphs.pass, "OK"); // glyphs swapped
    }
}
