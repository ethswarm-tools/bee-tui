//! In-memory ring buffer of HTTP-traffic log events, fed by a custom
//! `tracing-subscriber` layer that filters to the `bee::http` target.
//!
//! `bee::Inner::send` (in bee-rs ≥ 1.3.0) emits a single
//! `tracing::debug!` event per request carrying `method`, `url`,
//! `status`, and `elapsed_ms`. The S10 Command-log pane subscribes
//! to this buffer to render the lazygit-style request tail without
//! re-instrumenting any code.
//!
//! Architecture
//! - One process-wide [`LogCapture`] (initialised via [`install`]).
//! - `LogCapture::push` is called by [`CaptureLayer::on_event`].
//! - `LogCapture::snapshot` returns a `Vec<LogEntry>` cheap clone for
//!   rendering — the lock is held only to `clone` the deque.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use tracing::{Event, Level, Subscriber};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

/// One captured `bee::http` event.
#[derive(Clone, Debug)]
pub struct LogEntry {
    /// Pre-formatted UTC `HH:MM:SS` of capture (cheap to render).
    pub ts: String,
    pub method: String,
    pub url: String,
    pub status: Option<u16>,
    pub elapsed_ms: Option<u64>,
    /// Free-form message text from the event (e.g. "bee api request",
    /// "bee api error response").
    pub message: String,
}

/// Bounded ring buffer; cloning shares the underlying `Arc`.
#[derive(Clone, Debug)]
pub struct LogCapture {
    inner: Arc<Mutex<VecDeque<LogEntry>>>,
    capacity: usize,
}

impl LogCapture {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    /// Append one entry; evict oldest when full.
    pub fn push(&self, entry: LogEntry) {
        let mut buf = self.inner.lock().expect("log capture mutex poisoned");
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    /// Cheap snapshot for rendering — collects entries to a Vec under
    /// a brief lock and returns it.
    pub fn snapshot(&self) -> Vec<LogEntry> {
        let buf = self.inner.lock().expect("log capture mutex poisoned");
        buf.iter().cloned().collect()
    }
}

/// One captured cockpit-internal event (anything that is NOT
/// `bee::http`). Lives in a separate ring from [`LogEntry`] so the
/// cockpit-log tab and the bee::http tab are non-overlapping.
#[derive(Clone, Debug)]
pub struct CockpitEntry {
    pub ts: String,
    /// Severity level rendered as a fixed token (`ERROR`/`WARN`/`INFO`
    /// /`DEBUG`/`TRACE`). Pre-formatted so the renderer doesn't have
    /// to map levels.
    pub level: String,
    /// Tracing target — the module path the event came from. Helps
    /// readers tell `bee_tui::watch` from `bee_tui::supervisor` etc.
    pub target: String,
    /// Free-form message text.
    pub message: String,
}

/// Bounded ring buffer for cockpit-internal events.
#[derive(Clone, Debug)]
pub struct CockpitCapture {
    inner: Arc<Mutex<VecDeque<CockpitEntry>>>,
    capacity: usize,
}

impl CockpitCapture {
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(VecDeque::with_capacity(capacity))),
            capacity,
        }
    }

    pub fn push(&self, entry: CockpitEntry) {
        let mut buf = self.inner.lock().expect("cockpit capture mutex poisoned");
        if buf.len() == self.capacity {
            buf.pop_front();
        }
        buf.push_back(entry);
    }

    pub fn snapshot(&self) -> Vec<CockpitEntry> {
        let buf = self.inner.lock().expect("cockpit capture mutex poisoned");
        buf.iter().cloned().collect()
    }
}

/// Layer plugged into `tracing-subscriber::registry()` from
/// [`crate::logging::init`]. Splits events by target:
///   * `bee::http` → [`LogCapture`] (the bee::http tab).
///   * everything else → [`CockpitCapture`] (the new Cockpit tab).
///
/// One layer instead of two so a single `tracing` event walks the
/// subscriber chain just once.
pub struct CaptureLayer {
    capture: LogCapture,
    cockpit: CockpitCapture,
}

impl CaptureLayer {
    pub fn new(capture: LogCapture, cockpit: CockpitCapture) -> Self {
        Self { capture, cockpit }
    }
}

impl<S> Layer<S> for CaptureLayer
where
    S: Subscriber + for<'a> LookupSpan<'a>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let target = event.metadata().target();
        if target == "bee::http" {
            let mut v = FieldVisitor::default();
            event.record(&mut v);
            self.capture.push(LogEntry {
                ts: format_now_hms(),
                method: v.method.unwrap_or_default(),
                url: v.url.unwrap_or_default(),
                status: v.status,
                elapsed_ms: v.elapsed_ms,
                message: v.message.unwrap_or_default(),
            });
            return;
        }
        // Everything else lands on the Cockpit tab. Filter out events
        // emitted by tracing's own internals so the tab doesn't fill
        // up with subscriber-bookkeeping noise.
        if target.starts_with("tracing") {
            return;
        }
        let mut v = FieldVisitor::default();
        event.record(&mut v);
        self.cockpit.push(CockpitEntry {
            ts: format_now_hms(),
            level: format_level(*event.metadata().level()),
            target: target.to_string(),
            message: v.message.unwrap_or_default(),
        });
    }
}

fn format_level(l: Level) -> String {
    match l {
        Level::ERROR => "ERROR".into(),
        Level::WARN => "WARN".into(),
        Level::INFO => "INFO".into(),
        Level::DEBUG => "DEBUG".into(),
        Level::TRACE => "TRACE".into(),
    }
}

#[derive(Default)]
struct FieldVisitor {
    method: Option<String>,
    url: Option<String>,
    status: Option<u16>,
    elapsed_ms: Option<u64>,
    message: Option<String>,
}

impl tracing::field::Visit for FieldVisitor {
    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        match field.name() {
            "method" => self.method = Some(value.to_string()),
            "url" => self.url = Some(value.to_string()),
            _ => {}
        }
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        match field.name() {
            "status" => self.status = Some(value as u16),
            "elapsed_ms" => self.elapsed_ms = Some(value),
            _ => {}
        }
    }

    fn record_i64(&mut self, field: &tracing::field::Field, value: i64) {
        if value >= 0 {
            self.record_u64(field, value as u64);
        }
    }

    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.message = Some(format!("{value:?}").trim_matches('"').to_string());
        } else if field.name() == "method" && self.method.is_none() {
            self.method = Some(format!("{value:?}").trim_matches('"').to_string());
        } else if field.name() == "url" && self.url.is_none() {
            self.url = Some(format!("{value:?}").trim_matches('"').to_string());
        }
    }
}

static GLOBAL: OnceLock<LogCapture> = OnceLock::new();
static GLOBAL_COCKPIT: OnceLock<CockpitCapture> = OnceLock::new();

/// Construct the process-wide bee::http capture buffer and remember
/// it. Called once during [`crate::logging::init`]. Subsequent calls
/// return the already-installed capture.
pub fn install(capacity: usize) -> LogCapture {
    GLOBAL.get_or_init(|| LogCapture::new(capacity)).clone()
}

/// Construct the process-wide cockpit-internal capture buffer.
/// Capacity defaults large because cockpit events are bounded mostly
/// by the operator's own action volume, not by network chatter.
pub fn install_cockpit(capacity: usize) -> CockpitCapture {
    GLOBAL_COCKPIT
        .get_or_init(|| CockpitCapture::new(capacity))
        .clone()
}

/// Borrow the installed capture, if any. Returns `None` before
/// [`install`] has been called.
pub fn handle() -> Option<LogCapture> {
    GLOBAL.get().cloned()
}

/// Borrow the installed cockpit-capture, if any.
pub fn cockpit_handle() -> Option<CockpitCapture> {
    GLOBAL_COCKPIT.get().cloned()
}

fn format_now_hms() -> String {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default();
    let in_day = secs % 86_400;
    let h = in_day / 3600;
    let m = (in_day / 60) % 60;
    let s = in_day % 60;
    format!("{h:02}:{m:02}:{s:02}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_buffer_evicts_oldest() {
        let cap = LogCapture::new(2);
        for i in 0..3 {
            cap.push(LogEntry {
                ts: format!("00:00:{i:02}"),
                method: "GET".into(),
                url: format!("/{i}"),
                status: Some(200),
                elapsed_ms: Some(i),
                message: "test".into(),
            });
        }
        let snap = cap.snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].url, "/1");
        assert_eq!(snap[1].url, "/2");
    }

    #[test]
    fn install_returns_same_handle_on_second_call() {
        // Reset is hard for a OnceLock; rely on test ordering by using
        // a fresh capacity that any previous test won't have set.
        let a = install(123);
        let b = install(456);
        // Same Arc — installation is idempotent.
        assert!(Arc::ptr_eq(&a.inner, &b.inner));
    }

    #[test]
    fn format_now_hms_is_eight_chars() {
        assert_eq!(format_now_hms().len(), 8);
    }

    #[test]
    fn cockpit_ring_evicts_oldest() {
        let cap = CockpitCapture::new(2);
        for i in 0..3 {
            cap.push(CockpitEntry {
                ts: format!("00:00:{i:02}"),
                level: "INFO".into(),
                target: "bee_tui::test".into(),
                message: format!("msg {i}"),
            });
        }
        let snap = cap.snapshot();
        assert_eq!(snap.len(), 2);
        assert!(snap[0].message.contains("msg 1"));
        assert!(snap[1].message.contains("msg 2"));
    }

    #[test]
    fn install_cockpit_returns_same_handle() {
        let a = install_cockpit(321);
        let b = install_cockpit(654);
        assert!(Arc::ptr_eq(&a.inner, &b.inner));
    }

    #[test]
    fn format_level_round_trip() {
        assert_eq!(format_level(Level::ERROR), "ERROR");
        assert_eq!(format_level(Level::WARN), "WARN");
        assert_eq!(format_level(Level::INFO), "INFO");
        assert_eq!(format_level(Level::DEBUG), "DEBUG");
        assert_eq!(format_level(Level::TRACE), "TRACE");
    }
}
