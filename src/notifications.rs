//! Notification center — v1.14 "in-cockpit alerts."
//!
//! The cockpit already detects every event worth alerting on (gate
//! state machine in `crate::alerts`, fleet status transitions in
//! `crate::app::FleetAggregator`). Through v1.13 those events only
//! POSTed to a webhook — useful for Slack but invisible to an
//! operator looking at the cockpit. v1.14 adds three in-cockpit
//! sinks that share the same source pipeline:
//!
//! 1. **Toast overlay** — transient top-right corner card that
//!    auto-dismisses after [`NotificationsConfig::toast_seconds`].
//! 2. **History overlay** — `Ctrl+Alt+N` (or `:notifications`) opens
//!    a centered list of the last 200 notifications this session.
//! 3. **Desktop / terminal escalation** — opt-in via the
//!    `[notifications]` config block. `desktop = true` fires a
//!    libnotify-style OS notification through `notify-rust`;
//!    `bell = "fail"|"warn"` emits a terminal BEL on the matching
//!    severities.
//!
//! All three are best-effort. A libnotify call that fails (no dbus
//! session, unsupported platform, etc.) gets a single warn-level
//! log line and the rest of the pipeline keeps running.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use crate::config::NotificationsConfig;

/// Severity ladder for notifications. Maps 1:1 to the gate states
/// the alerter already uses, with `Recovery` added for the
/// `Fail → Pass` transitions that operators want to see ("we're
/// back").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotificationSeverity {
    /// Critical — a gate went red, a node went unreachable, a stamp
    /// crossed the 24-hour urgent threshold.
    Fail,
    /// Caution — a gate went amber, peer count dropped under the
    /// warn threshold, stamp TTL crossed 7-day planning threshold.
    Warn,
    /// Recovery — something that was Fail/Warn is back to Pass.
    /// Useful to know "the outage cleared."
    Recovery,
    /// Informational — currently unused but reserved for future
    /// non-state-machine events (e.g. "new bee-tui version
    /// available").
    Info,
}

impl NotificationSeverity {
    pub fn label(self) -> &'static str {
        match self {
            NotificationSeverity::Fail => "FAIL",
            NotificationSeverity::Warn => "WARN",
            NotificationSeverity::Recovery => "OK",
            NotificationSeverity::Info => "INFO",
        }
    }
}

/// One operator-facing notification. Pure data — the rendering
/// shape lives in `app.rs::draw_toasts` / `draw_notification_overlay`.
#[derive(Debug, Clone)]
pub struct Notification {
    /// When the notification was first ingested. Drives the
    /// "fired N min ago" string in the history overlay and the
    /// auto-dismiss timer for toasts.
    pub at: Instant,
    pub severity: NotificationSeverity,
    /// Short headline — fits in a single line of a 60-column toast.
    /// Example: `prod-eu: Pass → Fail` or `Stamp TTL → Warn`.
    pub headline: String,
    /// Optional why-text — operator-facing explanation. Drawn
    /// dimmed below the headline in the toast, and as a
    /// continuation line in the history overlay.
    pub why: Option<String>,
}

/// Capacity of the per-session ring buffer of past notifications.
/// 200 is enough to survive a noisy operator day without burning
/// memory; older notifications fall off the front.
const HISTORY_CAPACITY: usize = 200;
/// Maximum number of toasts visible at once in the top-right
/// stack. Newer toasts appear above older ones; once the cap is
/// hit, the oldest in-flight toast is evicted early to make room.
pub const MAX_VISIBLE_TOASTS: usize = 3;

/// State the notification center keeps across ticks.
#[derive(Debug, Default, Clone)]
pub struct NotificationCenter {
    /// Ring buffer of every notification fired this session, newest
    /// last. Surfaced by the history overlay.
    history: VecDeque<Notification>,
    /// Currently-visible toasts. Each has its own dismiss-at
    /// timestamp computed at ingest time using the configured
    /// `toast_seconds`. Drained on every tick by [`purge_expired`].
    toasts: VecDeque<(Notification, Instant)>,
}

impl NotificationCenter {
    /// Ingest one notification: append to history, push onto the
    /// visible toast stack (if toasts are enabled), and optionally
    /// fire the desktop notification + terminal bell escalations.
    /// Returns the toast-dismiss timestamp for tests; callers
    /// usually ignore it.
    pub fn ingest(
        &mut self,
        notification: Notification,
        cfg: &NotificationsConfig,
        now: Instant,
    ) -> Option<Instant> {
        // 1. Append to history (ring buffer).
        if self.history.len() >= HISTORY_CAPACITY {
            self.history.pop_front();
        }
        self.history.push_back(notification.clone());

        let mut dismiss_at = None;

        // 2. Push toast (in-cockpit transient overlay).
        if cfg.toast_enabled {
            let secs = cfg.toast_seconds.max(1);
            let due = now + Duration::from_secs(secs);
            if self.toasts.len() >= MAX_VISIBLE_TOASTS {
                self.toasts.pop_front();
            }
            self.toasts.push_back((notification.clone(), due));
            dismiss_at = Some(due);
        }

        // 3. Terminal BEL escalation. Only fires for the configured
        // severity threshold; stdout-side so a redirect to a file
        // doesn't see it (most operators redirect stderr).
        if should_bell(cfg, notification.severity) {
            // \x07 is the ASCII BEL — most terminal emulators flash
            // the title bar or play the system bell.
            use std::io::Write;
            let mut out = std::io::stderr();
            let _ = out.write_all(b"\x07");
            let _ = out.flush();
        }

        // 4. Desktop notification escalation. notify-rust calls
        // libnotify (Linux via zbus, no system lib dep), Notification
        // Center on macOS, Windows toast on Windows. Errors get a
        // single warn log and don't propagate — a missing dbus
        // session shouldn't kill the cockpit.
        if cfg.desktop && matches!(
            notification.severity,
            NotificationSeverity::Fail | NotificationSeverity::Warn
        ) {
            fire_desktop_notification(&notification);
        }

        dismiss_at
    }

    /// Drop toasts whose dismiss timestamp has elapsed. Called on
    /// every Tick from `App::handle_actions`. Pure — toasts are a
    /// `VecDeque`; we scan from the front.
    pub fn purge_expired(&mut self, now: Instant) {
        while let Some((_, due)) = self.toasts.front() {
            if *due <= now {
                self.toasts.pop_front();
            } else {
                break;
            }
        }
    }

    /// Snapshot of the visible toasts for rendering. Returns a
    /// cloned slice so the render closure doesn't borrow `self`.
    pub fn visible_toasts(&self) -> Vec<Notification> {
        self.toasts.iter().map(|(n, _)| n.clone()).collect()
    }

    /// Snapshot of the full history for the overlay. Newest first
    /// (callers usually want reverse chronological scan).
    pub fn history_newest_first(&self) -> Vec<Notification> {
        self.history.iter().rev().cloned().collect()
    }

    /// Count of notifications in the session history. Used by
    /// the overlay header.
    pub fn history_len(&self) -> usize {
        self.history.len()
    }
}

/// Decide whether the current notification severity should ring
/// the terminal bell, per the configured policy. Pure for tests.
pub fn should_bell(cfg: &NotificationsConfig, severity: NotificationSeverity) -> bool {
    match cfg.bell.as_str() {
        "fail" => matches!(severity, NotificationSeverity::Fail),
        "warn" => matches!(
            severity,
            NotificationSeverity::Fail | NotificationSeverity::Warn
        ),
        _ => false,
    }
}

fn fire_desktop_notification(n: &Notification) {
    let mut nb = notify_rust::Notification::new();
    nb.summary(&format!("bee-tui: {}", n.headline));
    if let Some(why) = &n.why {
        nb.body(why);
    }
    nb.appname("bee-tui");
    // No icon mapping yet — the OS default app icon is fine; an
    // explicit icon path adds platform-specific complexity (Linux
    // wants `bell` / `dialog-warning`, macOS uses the bundle).
    if let Err(e) = nb.show() {
        tracing::warn!(target: "bee_tui::notifications", "desktop notification failed: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::NotificationsConfig;

    fn cfg(toast: bool, seconds: u64, bell: &str, desktop: bool) -> NotificationsConfig {
        NotificationsConfig {
            toast_enabled: toast,
            toast_seconds: seconds,
            desktop,
            bell: bell.into(),
        }
    }

    fn notif(sev: NotificationSeverity, headline: &str) -> Notification {
        Notification {
            at: Instant::now(),
            severity: sev,
            headline: headline.into(),
            why: None,
        }
    }

    #[test]
    fn ingest_appends_to_history_and_pushes_toast_when_enabled() {
        let mut nc = NotificationCenter::default();
        let c = cfg(true, 5, "off", false);
        let now = Instant::now();
        let due = nc.ingest(notif(NotificationSeverity::Fail, "x"), &c, now);
        assert!(due.is_some());
        assert_eq!(nc.history_len(), 1);
        assert_eq!(nc.visible_toasts().len(), 1);
    }

    #[test]
    fn ingest_skips_toast_when_disabled() {
        let mut nc = NotificationCenter::default();
        let c = cfg(false, 5, "off", false);
        let due = nc.ingest(notif(NotificationSeverity::Fail, "x"), &c, Instant::now());
        assert!(due.is_none());
        // History still grows even when toasts are off — the
        // overlay should always have a complete record.
        assert_eq!(nc.history_len(), 1);
        assert!(nc.visible_toasts().is_empty());
    }

    #[test]
    fn history_capped_at_capacity() {
        let mut nc = NotificationCenter::default();
        let c = cfg(false, 5, "off", false);
        let now = Instant::now();
        for i in 0..(HISTORY_CAPACITY + 50) {
            nc.ingest(
                notif(NotificationSeverity::Info, &format!("event {i}")),
                &c,
                now,
            );
        }
        assert_eq!(nc.history_len(), HISTORY_CAPACITY);
        // Newest survived; oldest was evicted.
        let newest_first = nc.history_newest_first();
        assert!(newest_first[0].headline.contains(&format!(
            "event {}",
            HISTORY_CAPACITY + 50 - 1
        )));
    }

    #[test]
    fn visible_toasts_capped_at_max() {
        let mut nc = NotificationCenter::default();
        let c = cfg(true, 999_999, "off", false);
        let now = Instant::now();
        for i in 0..(MAX_VISIBLE_TOASTS + 2) {
            nc.ingest(
                notif(NotificationSeverity::Fail, &format!("event {i}")),
                &c,
                now,
            );
        }
        assert_eq!(nc.visible_toasts().len(), MAX_VISIBLE_TOASTS);
    }

    #[test]
    fn purge_expired_drops_only_expired_toasts() {
        let mut nc = NotificationCenter::default();
        let c = cfg(true, 5, "off", false);
        let t0 = Instant::now();
        // Toast A expires at t0+5s
        nc.ingest(notif(NotificationSeverity::Fail, "A"), &c, t0);
        // Toast B expires at t0+10+5=t0+15s
        nc.ingest(
            notif(NotificationSeverity::Fail, "B"),
            &c,
            t0 + Duration::from_secs(10),
        );
        // At t0+6s, A is gone, B remains.
        nc.purge_expired(t0 + Duration::from_secs(6));
        let toasts = nc.visible_toasts();
        assert_eq!(toasts.len(), 1);
        assert_eq!(toasts[0].headline, "B");
    }

    #[test]
    fn should_bell_respects_threshold() {
        let off = cfg(true, 5, "off", false);
        let fail_only = cfg(true, 5, "fail", false);
        let warn_and_fail = cfg(true, 5, "warn", false);
        // off → never
        assert!(!should_bell(&off, NotificationSeverity::Fail));
        assert!(!should_bell(&off, NotificationSeverity::Warn));
        // fail → only Fail
        assert!(should_bell(&fail_only, NotificationSeverity::Fail));
        assert!(!should_bell(&fail_only, NotificationSeverity::Warn));
        assert!(!should_bell(&fail_only, NotificationSeverity::Recovery));
        // warn → Fail + Warn
        assert!(should_bell(&warn_and_fail, NotificationSeverity::Fail));
        assert!(should_bell(&warn_and_fail, NotificationSeverity::Warn));
        assert!(!should_bell(&warn_and_fail, NotificationSeverity::Recovery));
    }

    #[test]
    fn history_newest_first_reverses_insertion() {
        let mut nc = NotificationCenter::default();
        let c = cfg(false, 5, "off", false);
        let now = Instant::now();
        nc.ingest(notif(NotificationSeverity::Fail, "first"), &c, now);
        nc.ingest(notif(NotificationSeverity::Warn, "second"), &c, now);
        nc.ingest(notif(NotificationSeverity::Recovery, "third"), &c, now);
        let list = nc.history_newest_first();
        assert_eq!(list[0].headline, "third");
        assert_eq!(list[1].headline, "second");
        assert_eq!(list[2].headline, "first");
    }

    #[test]
    fn severity_label_is_stable() {
        assert_eq!(NotificationSeverity::Fail.label(), "FAIL");
        assert_eq!(NotificationSeverity::Warn.label(), "WARN");
        assert_eq!(NotificationSeverity::Recovery.label(), "OK");
        assert_eq!(NotificationSeverity::Info.label(), "INFO");
    }
}
