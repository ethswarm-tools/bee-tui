//! Tabbed bottom-pane that replaces the previous single-stream
//! `bee::http` tail. Six tabs split the log space along two axes:
//!
//! 1. **Bee severity tabs** — Errors / Warning / Info / Debug. Filled
//!    by parsing the supervised Bee node's stdout (increment 3).
//! 2. **Bee HTTP tab** — the served-request log line filtered out of
//!    the same Bee stream (increment 4).
//! 3. **bee::http tab** — bee-tui's *own* outbound calls (the legacy
//!    `CommandLog` view). Kept as a tab because it's still the trust
//!    anchor for every gauge in the cockpit.
//!
//! Increment 2 (this file) ships the UI scaffolding: tab state
//! machine, ring buffers per tab, height-clamping resize, and
//! state persistence for the operator's last height + active tab.
//! The four Bee-fed tabs render an "(awaiting bee log...)" placeholder
//! until the supervisor wires real entries through.

use std::collections::VecDeque;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::Component;
use crate::action::Action;
use crate::log_capture::{LogCapture, LogEntry};
use crate::state::{LOG_PANE_MAX_HEIGHT, LOG_PANE_MIN_HEIGHT};
use crate::theme;

/// Capacity of each Bee-side tab's ring buffer. Generous enough to
/// catch a burst of errors while staying memory-cheap.
const BEE_TAB_RING_CAPACITY: usize = 500;

/// The six tabs, in display order. Order is `Errors → Debug` so the
/// most operator-relevant severity is leftmost (matches reading
/// direction).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LogTab {
    Errors,
    Warning,
    Info,
    Debug,
    BeeHttp,
    SelfHttp,
}

impl LogTab {
    pub const ALL: [LogTab; 6] = [
        LogTab::Errors,
        LogTab::Warning,
        LogTab::Info,
        LogTab::Debug,
        LogTab::BeeHttp,
        LogTab::SelfHttp,
    ];

    /// Parse the kebab-case form persisted in `state.toml`. Unknown
    /// strings → SelfHttp (the only tab guaranteed to have data
    /// regardless of whether [bee] is configured).
    pub fn from_kebab(s: &str) -> Self {
        match s {
            "errors" => Self::Errors,
            "warning" => Self::Warning,
            "info" => Self::Info,
            "debug" => Self::Debug,
            "bee-http" => Self::BeeHttp,
            "self-http" => Self::SelfHttp,
            _ => Self::SelfHttp,
        }
    }

    pub fn to_kebab(self) -> &'static str {
        match self {
            Self::Errors => "errors",
            Self::Warning => "warning",
            Self::Info => "info",
            Self::Debug => "debug",
            Self::BeeHttp => "bee-http",
            Self::SelfHttp => "self-http",
        }
    }

    /// Short label rendered in the tab strip.
    pub fn label(self) -> &'static str {
        match self {
            Self::Errors => "Errors",
            Self::Warning => "Warn",
            Self::Info => "Info",
            Self::Debug => "Debug",
            Self::BeeHttp => "Bee HTTP",
            Self::SelfHttp => "bee::http",
        }
    }

    /// Index into [`LogTab::ALL`].
    fn index(self) -> usize {
        Self::ALL.iter().position(|t| *t == self).unwrap_or(5)
    }

    fn from_index(i: usize) -> Self {
        Self::ALL.get(i).copied().unwrap_or(Self::SelfHttp)
    }
}

/// Single line on a Bee-side tab. The supervisor's log tailer
/// (increment 3) builds these from parsed Bee log entries; for now
/// the structure exists so the renderer + key handling can be
/// validated end-to-end against synthetic inputs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeeLogLine {
    /// `time` field from the source log line, rendered verbatim.
    pub timestamp: String,
    /// `logger` field — usually `node/<subsystem>`.
    pub logger: String,
    /// `msg` field plus any extra key-value pairs the parser kept.
    pub message: String,
}

/// Renderable row inside the active tab. Pure data so the renderer
/// stays straightforward and snapshot tests can lock layout.
pub enum LogRow<'a> {
    Self_(&'a LogEntry),
    Bee(&'a BeeLogLine),
}

/// Buffers feeding the Bee-side tabs. Each tab has its own ring
/// buffer so a noisy debug stream never evicts a precious error line.
#[derive(Debug, Default)]
pub struct BeeLogBuffers {
    pub errors: VecDeque<BeeLogLine>,
    pub warning: VecDeque<BeeLogLine>,
    pub info: VecDeque<BeeLogLine>,
    pub debug: VecDeque<BeeLogLine>,
    pub bee_http: VecDeque<BeeLogLine>,
}

impl BeeLogBuffers {
    fn buffer_for(&self, tab: LogTab) -> Option<&VecDeque<BeeLogLine>> {
        match tab {
            LogTab::Errors => Some(&self.errors),
            LogTab::Warning => Some(&self.warning),
            LogTab::Info => Some(&self.info),
            LogTab::Debug => Some(&self.debug),
            LogTab::BeeHttp => Some(&self.bee_http),
            LogTab::SelfHttp => None,
        }
    }

    /// Number of entries on a given tab. Used by the tab strip's
    /// counter chips ("Errors 3" etc).
    pub fn count(&self, tab: LogTab) -> usize {
        self.buffer_for(tab).map(|b| b.len()).unwrap_or(0)
    }
}

/// The component itself. Owns the tab state + ring buffers; reads
/// the bee::http capture from a borrowed handle, same as the legacy
/// CommandLog did.
pub struct LogPane {
    capture: Option<LogCapture>,
    self_http_entries: Vec<LogEntry>,
    bee_buffers: BeeLogBuffers,
    active_tab: LogTab,
    /// Height in lines including the title strip + borders.
    height: u16,
    /// Set by [`spawn_active`] when bee-tui is the supervisor — toggles
    /// placeholder text on the Bee-side tabs from "configure [bee]"
    /// to "(awaiting first log line)".
    spawn_active: bool,
}

impl LogPane {
    pub fn new(capture: Option<LogCapture>, initial_tab: LogTab, initial_height: u16) -> Self {
        Self {
            capture,
            self_http_entries: Vec::new(),
            bee_buffers: BeeLogBuffers::default(),
            active_tab: initial_tab,
            height: initial_height.clamp(LOG_PANE_MIN_HEIGHT, LOG_PANE_MAX_HEIGHT),
            spawn_active: false,
        }
    }

    pub fn active_tab(&self) -> LogTab {
        self.active_tab
    }

    pub fn height(&self) -> u16 {
        self.height
    }

    /// Tell the pane bee-tui is acting as the supervisor (so the
    /// placeholder changes from "configure [bee]" to "(awaiting first
    /// log line)"). Called once at startup; cheap to call repeatedly.
    pub fn set_spawn_active(&mut self, active: bool) {
        self.spawn_active = active;
    }

    /// Cycle to the next tab (left → right, wrapping). Returns the
    /// new active tab so callers can persist state without re-reading.
    pub fn next_tab(&mut self) -> LogTab {
        let i = (self.active_tab.index() + 1) % LogTab::ALL.len();
        self.active_tab = LogTab::from_index(i);
        self.active_tab
    }

    /// Cycle to the previous tab (right → left, wrapping).
    pub fn prev_tab(&mut self) -> LogTab {
        let len = LogTab::ALL.len();
        let i = (self.active_tab.index() + len - 1) % len;
        self.active_tab = LogTab::from_index(i);
        self.active_tab
    }

    /// Grow the pane by one line. Returns the new height. No-op once
    /// the cap is hit.
    pub fn grow(&mut self) -> u16 {
        self.height = (self.height + 1).min(LOG_PANE_MAX_HEIGHT);
        self.height
    }

    /// Shrink the pane by one line. Returns the new height. No-op
    /// once the floor is hit.
    pub fn shrink(&mut self) -> u16 {
        self.height = self.height.saturating_sub(1).max(LOG_PANE_MIN_HEIGHT);
        self.height
    }

    /// Push a Bee log line to the appropriate tab. The supervisor's
    /// log tailer (increment 3) calls this for each parsed line.
    /// Bounded — when the ring is full the oldest entry is evicted.
    pub fn push_bee(&mut self, tab: LogTab, line: BeeLogLine) {
        let buf = match tab {
            LogTab::Errors => &mut self.bee_buffers.errors,
            LogTab::Warning => &mut self.bee_buffers.warning,
            LogTab::Info => &mut self.bee_buffers.info,
            LogTab::Debug => &mut self.bee_buffers.debug,
            LogTab::BeeHttp => &mut self.bee_buffers.bee_http,
            LogTab::SelfHttp => return, // not a bee-side tab
        };
        if buf.len() == BEE_TAB_RING_CAPACITY {
            buf.pop_front();
        }
        buf.push_back(line);
    }

    fn pull_self_http(&mut self) {
        if let Some(c) = &self.capture {
            self.self_http_entries = c.snapshot();
        }
    }
}

impl Component for LogPane {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_self_http();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let t = theme::active();
        let active = self.active_tab;
        let block = Block::default().borders(Borders::ALL).title(tab_title_line(
            active,
            &self.bee_buffers,
            t,
        ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // Single content row — the inner area minus the borders. The
        // tab strip lives in the title, so we get one extra line for
        // payload vs. the legacy layout.
        let content_area = Layout::vertical([Constraint::Min(0)])
            .split(inner)
            .first()
            .copied()
            .unwrap_or(inner);

        let lines: Vec<Line> = match active {
            LogTab::SelfHttp => render_self_http(&self.self_http_entries, t),
            tab => render_bee_tab(&self.bee_buffers, tab, self.spawn_active, t),
        };

        // Show the most recent N lines (newest at bottom — tail
        // semantics). Empty state already produced its own line.
        let take_last = content_area.height as usize;
        let visible: Vec<Line> = if lines.len() > take_last {
            let skip = lines.len() - take_last;
            lines.into_iter().skip(skip).collect()
        } else {
            lines
        };

        frame.render_widget(Paragraph::new(visible), content_area);
        Ok(())
    }
}

/// Build the `[Errors 3] [Warn 0] [Info 247] [Debug 1.2k] [Bee HTTP] [bee::http]`
/// title strip with the active tab highlighted and counters from the
/// per-tab buffers. Counters above 999 collapse to `1.2k` style so
/// the strip fits an 80-column terminal.
fn tab_title_line<'a>(active: LogTab, bufs: &BeeLogBuffers, t: &theme::Theme) -> Line<'a> {
    let mut spans: Vec<Span> = Vec::with_capacity(LogTab::ALL.len() * 2 + 1);
    spans.push(Span::raw(" "));
    for tab in LogTab::ALL {
        let count = bufs.count(tab);
        let label = if count == 0 || tab == LogTab::SelfHttp {
            format!(" {} ", tab.label())
        } else {
            format!(" {} {} ", tab.label(), human_count(count))
        };
        let style = if tab == active {
            Style::default()
                .fg(t.tab_active_fg)
                .bg(t.tab_active_bg)
                .add_modifier(Modifier::BOLD)
        } else {
            tab_severity_color(tab, t)
        };
        spans.push(Span::styled(label, style));
        spans.push(Span::raw(" "));
    }
    Line::from(spans)
}

/// Subtle per-tab tint on the inactive label so Errors stand out
/// even when the operator is on a different tab.
fn tab_severity_color(tab: LogTab, t: &theme::Theme) -> Style {
    match tab {
        LogTab::Errors => Style::default().fg(t.fail),
        LogTab::Warning => Style::default().fg(t.warn),
        LogTab::Info => Style::default().fg(t.info),
        LogTab::Debug => Style::default().fg(t.dim),
        LogTab::BeeHttp => Style::default().fg(t.accent),
        LogTab::SelfHttp => Style::default().fg(t.dim),
    }
}

fn human_count(n: usize) -> String {
    if n < 1_000 {
        n.to_string()
    } else if n < 1_000_000 {
        format!("{:.1}k", n as f64 / 1000.0)
    } else {
        format!("{:.1}m", n as f64 / 1_000_000.0)
    }
}

fn render_self_http<'a>(entries: &'a [LogEntry], t: &theme::Theme) -> Vec<Line<'a>> {
    if entries.is_empty() {
        return vec![Line::from(Span::styled(
            "  (waiting for first request…)",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ))];
    }
    entries.iter().map(|e| self_http_line(e, t)).collect()
}

fn render_bee_tab<'a>(
    bufs: &'a BeeLogBuffers,
    tab: LogTab,
    spawn_active: bool,
    t: &theme::Theme,
) -> Vec<Line<'a>> {
    let buf = match bufs.buffer_for(tab) {
        Some(b) => b,
        None => return Vec::new(),
    };
    if buf.is_empty() {
        let msg = if spawn_active {
            "  (awaiting bee log entries on this severity…)"
        } else {
            "  (no bee child — set [bee] in config or pass --bee-bin / --bee-config)"
        };
        return vec![Line::from(Span::styled(
            msg,
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ))];
    }
    buf.iter()
        .map(|line| {
            Line::from(vec![
                Span::styled(format!("{} ", line.timestamp), Style::default().fg(t.dim)),
                Span::styled(
                    format!("{:<22}", line.logger),
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::raw(line.message.clone()),
            ])
        })
        .collect()
}

fn self_http_line<'a>(e: &'a LogEntry, t: &theme::Theme) -> Line<'a> {
    let status_style = match e.status {
        Some(s) if (200..300).contains(&s) => Style::default().fg(t.pass),
        Some(s) if (300..400).contains(&s) => Style::default().fg(t.info),
        Some(s) if (400..500).contains(&s) => Style::default().fg(t.warn),
        Some(_) => Style::default().fg(t.fail),
        None => Style::default().fg(t.dim),
    };
    let method_style = Style::default()
        .fg(method_color(&e.method))
        .add_modifier(Modifier::BOLD);
    let elapsed = e
        .elapsed_ms
        .map(|ms| format!("{ms:>4}ms"))
        .unwrap_or_else(|| "    —".into());
    let path = path_only(&e.url);
    Line::from(vec![
        Span::styled(format!("{} ", e.ts), Style::default().fg(t.dim)),
        Span::styled(format!("{:<5}", e.method), method_style),
        Span::raw(" "),
        Span::raw(path),
        Span::raw("  "),
        Span::styled(
            e.status
                .map(|s| s.to_string())
                .unwrap_or_else(|| "—".into()),
            status_style,
        ),
        Span::raw("  "),
        Span::styled(elapsed, Style::default().fg(t.dim)),
    ])
}

/// Per-method colour, lazygit-style. Same palette as the legacy
/// CommandLog so the bee::http tab keeps its identity after the
/// move into the tabbed pane.
fn method_color(method: &str) -> Color {
    match method {
        "GET" => Color::Blue,
        "POST" => Color::Green,
        "PUT" => Color::Yellow,
        "DELETE" => Color::Red,
        "PATCH" => Color::Magenta,
        "HEAD" => Color::Cyan,
        _ => Color::White,
    }
}

/// Drop scheme + host from the URL so the tail stays readable on
/// 80-col terminals. `http://localhost:1633/health` → `/health`.
/// Matches the legacy CommandLog implementation; tests live here too.
fn path_only(url: &str) -> String {
    if let Some(rest) = url.split_once("//").and_then(|(_, r)| r.split_once('/')) {
        format!("/{}", rest.1)
    } else {
        url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn next_tab_wraps() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        for expected in [
            LogTab::Warning,
            LogTab::Info,
            LogTab::Debug,
            LogTab::BeeHttp,
            LogTab::SelfHttp,
            LogTab::Errors,
        ] {
            assert_eq!(pane.next_tab(), expected);
        }
    }

    #[test]
    fn prev_tab_wraps() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        for expected in [
            LogTab::SelfHttp,
            LogTab::BeeHttp,
            LogTab::Debug,
            LogTab::Info,
            LogTab::Warning,
            LogTab::Errors,
        ] {
            assert_eq!(pane.prev_tab(), expected);
        }
    }

    #[test]
    fn grow_clamps_at_max() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MAX_HEIGHT - 1);
        pane.grow();
        assert_eq!(pane.height(), LOG_PANE_MAX_HEIGHT);
        // Further grows are no-ops.
        pane.grow();
        pane.grow();
        assert_eq!(pane.height(), LOG_PANE_MAX_HEIGHT);
    }

    #[test]
    fn shrink_clamps_at_min() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT + 1);
        pane.shrink();
        assert_eq!(pane.height(), LOG_PANE_MIN_HEIGHT);
        pane.shrink();
        pane.shrink();
        assert_eq!(pane.height(), LOG_PANE_MIN_HEIGHT);
    }

    #[test]
    fn ring_capacity_is_enforced() {
        let mut pane = LogPane::new(None, LogTab::Debug, 10);
        // Push 600 entries — the ring should keep the most recent 500.
        for i in 0..(BEE_TAB_RING_CAPACITY + 100) {
            pane.push_bee(
                LogTab::Debug,
                BeeLogLine {
                    timestamp: format!("t{i}"),
                    logger: "node/test".into(),
                    message: format!("msg {i}"),
                },
            );
        }
        assert_eq!(pane.bee_buffers.debug.len(), BEE_TAB_RING_CAPACITY);
        assert_eq!(pane.bee_buffers.debug.front().unwrap().timestamp, "t100");
        assert_eq!(
            pane.bee_buffers.debug.back().unwrap().timestamp,
            format!("t{}", BEE_TAB_RING_CAPACITY + 99)
        );
    }

    #[test]
    fn push_bee_to_self_http_is_noop() {
        // Defensive: only the bee-side severities have buffers; the
        // SelfHttp tab is fed by the LogCapture. push_bee on SelfHttp
        // must silently drop, not panic.
        let mut pane = LogPane::new(None, LogTab::SelfHttp, 10);
        pane.push_bee(
            LogTab::SelfHttp,
            BeeLogLine {
                timestamp: "t".into(),
                logger: "x".into(),
                message: "m".into(),
            },
        );
        for tab in LogTab::ALL {
            assert_eq!(pane.bee_buffers.count(tab), 0, "tab {tab:?} got an entry");
        }
    }

    #[test]
    fn human_count_formats_thousands() {
        assert_eq!(human_count(0), "0");
        assert_eq!(human_count(42), "42");
        assert_eq!(human_count(999), "999");
        assert_eq!(human_count(1000), "1.0k");
        assert_eq!(human_count(1234), "1.2k");
        assert_eq!(human_count(999_999), "1000.0k");
        assert_eq!(human_count(1_000_000), "1.0m");
    }

    #[test]
    fn from_kebab_unknown_falls_back_to_self_http() {
        // Defensive: a hand-edited state.toml with a future tab name
        // shouldn't crash startup, just silently snap to a known good.
        assert_eq!(LogTab::from_kebab("future-tab"), LogTab::SelfHttp);
        assert_eq!(LogTab::from_kebab(""), LogTab::SelfHttp);
    }

    #[test]
    fn kebab_round_trips() {
        for tab in LogTab::ALL {
            assert_eq!(LogTab::from_kebab(tab.to_kebab()), tab);
        }
    }

    #[test]
    fn path_only_strips_scheme_and_host() {
        assert_eq!(path_only("http://localhost:1633/status"), "/status");
        assert_eq!(
            path_only("https://bee.example.com:1633/stamps/abc"),
            "/stamps/abc"
        );
    }

    #[test]
    fn path_only_handles_root_only() {
        assert_eq!(path_only("http://localhost:1633"), "http://localhost:1633");
    }
}
