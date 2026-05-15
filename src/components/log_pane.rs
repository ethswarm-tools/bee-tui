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
use crate::log_capture::{CockpitCapture, CockpitEntry, LogCapture, LogEntry};
use crate::state::{LOG_PANE_MAX_HEIGHT, LOG_PANE_MIN_HEIGHT};
use crate::theme;

/// Capacity of each Bee-side tab's ring buffer. Generous enough to
/// catch a burst of errors while staying memory-cheap.
const BEE_TAB_RING_CAPACITY: usize = 500;

// `LogTab` (enum + impl) and `BeeLogLine` (struct) now live in
// `bee_cockpit_core::bee_log` so the parser + tailer in core can use
// them without depending on the renderer. Re-exported here so existing
// `components::log_pane::LogTab` / `BeeLogLine` paths inside bee-tui
// keep working.
pub use bee_cockpit_core::bee_log::{BeeLogLine, LogTab};

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
            LogTab::SelfHttp | LogTab::Cockpit => None,
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
    cockpit_capture: Option<CockpitCapture>,
    cockpit_entries: Vec<CockpitEntry>,
    bee_buffers: BeeLogBuffers,
    active_tab: LogTab,
    /// Height in lines including the title strip + borders.
    height: u16,
    /// Set by [`spawn_active`] when bee-tui is the supervisor — toggles
    /// placeholder text on the Bee-side tabs from "configure [bee]"
    /// to "(awaiting first log line)".
    spawn_active: bool,
    /// Scroll offset for the active tab, in lines from the bottom.
    /// 0 = auto-tail (default; latest entries at the bottom). When
    /// non-zero, new entries arriving auto-bump the offset to keep
    /// the visible window stable. Reset to 0 on tab switch.
    scroll_offset: usize,
    /// Horizontal scroll offset in characters. Bee log lines often
    /// run past the pane width; this lets the operator pan right to
    /// see the truncated tail. Reset on tab switch.
    h_scroll_offset: u16,
    /// Active case-insensitive substring filter. `None` means no
    /// filter; rendered lines pass through unchanged. `Some(q)`
    /// hides every line whose lossless-stringified form does not
    /// contain `q` (lowercased on both sides). Survives tab
    /// switches deliberately — operators searching for a string
    /// want it applied to whichever tab they're looking at.
    filter: Option<String>,
    /// When `Some(buf)`, the in-pane filter prompt is open and the
    /// operator is typing `buf` into it. Distinct from `filter` so
    /// the live preview can show match-count for the buffer as
    /// they type (without re-committing on every keystroke).
    /// Committed into `filter` on Enter.
    filter_prompt: Option<String>,
    /// Operator-facing explanation shown on the empty Bee-side tabs
    /// when log auto-discovery found a local Bee but *can't* capture
    /// its log (e.g. it logs to a bare terminal). `None` falls back
    /// to the generic "no bee log source" placeholder.
    log_source_hint: Option<String>,
}

impl LogPane {
    pub fn new(capture: Option<LogCapture>, initial_tab: LogTab, initial_height: u16) -> Self {
        Self {
            capture,
            self_http_entries: Vec::new(),
            cockpit_capture: None,
            cockpit_entries: Vec::new(),
            bee_buffers: BeeLogBuffers::default(),
            active_tab: initial_tab,
            height: initial_height.clamp(LOG_PANE_MIN_HEIGHT, LOG_PANE_MAX_HEIGHT),
            spawn_active: false,
            scroll_offset: 0,
            h_scroll_offset: 0,
            filter: None,
            filter_prompt: None,
            log_source_hint: None,
        }
    }

    /// Attach the cockpit-capture ring buffer so the Cockpit tab can
    /// render events bee-tui itself emitted (everything that isn't
    /// `bee::http`). Wired by [`App::new`] after
    /// [`crate::logging::init`] has installed the capture.
    pub fn set_cockpit_capture(&mut self, cap: CockpitCapture) {
        self.cockpit_capture = Some(cap);
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

    /// Set the operator-facing hint shown on empty Bee-side tabs when
    /// a local Bee was found but its log can't be captured. `None`
    /// clears it (back to the generic placeholder). Set by `App` from
    /// log auto-discovery at startup and on every `:context` switch.
    pub fn set_log_source_hint(&mut self, hint: Option<String>) {
        self.log_source_hint = hint;
    }

    /// Cycle to the next tab (left → right, wrapping). Returns the
    /// new active tab so callers can persist state without re-reading.
    /// Resets the scroll offset — the new tab's content has nothing
    /// to do with where we were on the old one.
    pub fn next_tab(&mut self) -> LogTab {
        let i = (self.active_tab.index() + 1) % LogTab::ALL.len();
        self.active_tab = LogTab::from_index(i);
        self.scroll_offset = 0;
        self.h_scroll_offset = 0;
        self.active_tab
    }

    /// Cycle to the previous tab (right → left, wrapping).
    pub fn prev_tab(&mut self) -> LogTab {
        let len = LogTab::ALL.len();
        let i = (self.active_tab.index() + len - 1) % len;
        self.active_tab = LogTab::from_index(i);
        self.scroll_offset = 0;
        self.h_scroll_offset = 0;
        self.active_tab
    }

    /// Scroll the active tab up by `lines` (toward older entries).
    /// Clamped at draw-time to the buffer length so the user can't
    /// scroll past the top. `lines = 1` is the per-keystroke step;
    /// callers can pass a larger value for page-scrolling.
    pub fn scroll_up(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_add(lines);
    }

    /// Scroll the active tab down by `lines` (toward newer entries /
    /// the tail). Saturates at 0, which is the auto-tail state.
    pub fn scroll_down(&mut self, lines: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(lines);
    }

    /// Snap back to auto-tail mode (scroll_offset = 0). The pane
    /// resumes following new entries as they arrive. Also resets
    /// horizontal pan because operators usually want both axes
    /// reset together when "going back to live."
    pub fn resume_tail(&mut self) {
        self.scroll_offset = 0;
        self.h_scroll_offset = 0;
    }

    /// Pan the active tab right by `cols` characters. Bee log lines
    /// often run past the pane width; ratatui truncates them at the
    /// right edge by default so we add a horizontal scroll to let
    /// operators read the tail.
    pub fn scroll_right(&mut self, cols: u16) {
        self.h_scroll_offset = self.h_scroll_offset.saturating_add(cols);
    }

    /// Pan the active tab left by `cols` characters. Saturates at 0
    /// (the natural left edge).
    pub fn scroll_left(&mut self, cols: u16) {
        self.h_scroll_offset = self.h_scroll_offset.saturating_sub(cols);
    }

    /// Reset horizontal pan to the left edge without touching the
    /// vertical scroll. Used when operators want the line start
    /// back without leaving the historical window.
    pub fn reset_h_scroll(&mut self) {
        self.h_scroll_offset = 0;
    }

    /// Current horizontal scroll offset. Exposed for tests + the
    /// title-strip indicator.
    /// True while `/` has opened the in-pane filter prompt and the
    /// operator is typing. App's key router needs to know this so
    /// `j`/`k`/`q`/etc. don't escape into screen-level bindings
    /// while the prompt has focus.
    pub fn filter_prompt_visible(&self) -> bool {
        self.filter_prompt.is_some()
    }

    /// Open the filter prompt seeded with the existing filter (if
    /// any) so editing an active filter doesn't require retyping.
    pub fn open_filter_prompt(&mut self) {
        self.filter_prompt = Some(self.filter.clone().unwrap_or_default());
    }

    /// Drop the in-progress filter prompt without committing.
    /// Active filter (if any) is preserved.
    pub fn cancel_filter_prompt(&mut self) {
        self.filter_prompt = None;
    }

    /// Commit the in-progress prompt as the active filter. An
    /// empty buffer clears the filter (same as pressing Esc on a
    /// fresh prompt) — saves the operator a separate "clear"
    /// keystroke.
    pub fn commit_filter_prompt(&mut self) {
        if let Some(buf) = self.filter_prompt.take() {
            let trimmed = buf.trim();
            if trimmed.is_empty() {
                self.filter = None;
            } else {
                self.filter = Some(trimmed.to_string());
            }
        }
        // Filter changes invalidate scroll offset (the visible
        // line set just changed). Snap back to tail so the
        // operator sees the most recent matches.
        self.scroll_offset = 0;
        self.h_scroll_offset = 0;
    }

    /// Append a character to the filter prompt buffer.
    pub fn push_filter_char(&mut self, c: char) {
        if let Some(buf) = self.filter_prompt.as_mut() {
            buf.push(c);
        }
    }

    /// Delete the trailing character of the filter prompt buffer.
    pub fn pop_filter_char(&mut self) {
        if let Some(buf) = self.filter_prompt.as_mut() {
            buf.pop();
        }
    }

    /// Operator-facing view of the in-progress prompt buffer
    /// (e.g. for rendering). Empty string when the prompt isn't
    /// open.
    pub fn filter_prompt_buffer(&self) -> &str {
        self.filter_prompt.as_deref().unwrap_or("")
    }

    /// Currently committed filter string, if any.
    pub fn active_filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// Clear any active filter and dismiss the prompt. Wired to
    /// Esc when the prompt isn't open but a filter is active, so
    /// the same key clears both states.
    pub fn clear_filter(&mut self) {
        self.filter = None;
        self.filter_prompt = None;
        self.scroll_offset = 0;
        self.h_scroll_offset = 0;
    }

    pub fn h_scroll_offset(&self) -> u16 {
        self.h_scroll_offset
    }

    /// `true` when the pane is auto-tailing (the default state).
    pub fn is_tailing(&self) -> bool {
        self.scroll_offset == 0
    }

    /// Lines the pane is currently scrolled back from the tail.
    /// Useful for rendering "[paused N]" indicators in the title.
    pub fn scroll_offset(&self) -> usize {
        self.scroll_offset
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
    /// log tailer calls this for each parsed line. Bounded — when
    /// the ring is full the oldest entry is evicted.
    ///
    /// Scroll-stability: if this push lands on the *active* tab
    /// AND we're currently scrolled back (not auto-tailing), bump
    /// `scroll_offset` so the visible window stays anchored on the
    /// same content rather than drifting upward as new lines push
    /// the old ones up.
    pub fn push_bee(&mut self, tab: LogTab, line: BeeLogLine) {
        let buf = match tab {
            LogTab::Errors => &mut self.bee_buffers.errors,
            LogTab::Warning => &mut self.bee_buffers.warning,
            LogTab::Info => &mut self.bee_buffers.info,
            LogTab::Debug => &mut self.bee_buffers.debug,
            LogTab::BeeHttp => &mut self.bee_buffers.bee_http,
            LogTab::SelfHttp | LogTab::Cockpit => return, // capture-fed tabs
        };
        let was_full = buf.len() == BEE_TAB_RING_CAPACITY;
        if was_full {
            buf.pop_front();
        }
        buf.push_back(line);
        // Stabilise the user's view if they're scrolled back on
        // this same tab. When the ring is already full the eviction
        // already shifted our content by 1, so the offset doesn't
        // need to bump — the visible range stays in place.
        if tab == self.active_tab && self.scroll_offset > 0 && !was_full {
            self.scroll_offset = self.scroll_offset.saturating_add(1);
        }
    }

    fn pull_self_http(&mut self) {
        if let Some(c) = &self.capture {
            let new = c.snapshot();
            // Same stability logic as push_bee: when the operator
            // is scrolled back on the SelfHttp tab and the capture
            // grew by N entries, bump the offset by N so the visible
            // range doesn't drift.
            if self.active_tab == LogTab::SelfHttp && self.scroll_offset > 0 {
                let delta = new.len().saturating_sub(self.self_http_entries.len());
                if delta > 0 {
                    self.scroll_offset = self.scroll_offset.saturating_add(delta);
                }
            }
            self.self_http_entries = new;
        }
    }

    fn pull_cockpit(&mut self) {
        if let Some(c) = &self.cockpit_capture {
            let new = c.snapshot();
            if self.active_tab == LogTab::Cockpit && self.scroll_offset > 0 {
                let delta = new.len().saturating_sub(self.cockpit_entries.len());
                if delta > 0 {
                    self.scroll_offset = self.scroll_offset.saturating_add(delta);
                }
            }
            self.cockpit_entries = new;
        }
    }
}

impl Component for LogPane {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_self_http();
            self.pull_cockpit();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let t = theme::active();
        let active = self.active_tab;

        // Render every line for the active tab, then apply the
        // active filter (if any). The filter check looks at the
        // line's lossless plaintext — we keep span styling for the
        // surviving lines, so colour treatment of error/warn levels
        // is preserved across the filter.
        let raw_lines: Vec<Line> = match active {
            LogTab::SelfHttp => render_self_http(&self.self_http_entries, t),
            LogTab::Cockpit => render_cockpit(&self.cockpit_entries, t),
            tab => render_bee_tab(
                &self.bee_buffers,
                tab,
                self.spawn_active,
                self.log_source_hint.as_deref(),
                t,
            ),
        };
        let filter_active = self.filter.as_deref();
        let lines: Vec<Line> = match filter_active {
            None => raw_lines,
            Some(q) => filter_lines(raw_lines, q),
        };
        let match_count = if filter_active.is_some() {
            Some(lines.len())
        } else {
            None
        };

        // Clamp the scroll offset against what the active tab can
        // actually scroll. Pane content area excludes top + bottom
        // borders; we approximate from the outer area here.
        let content_h = (area.height as usize).saturating_sub(2);
        let max_offset = lines.len().saturating_sub(content_h);
        if self.scroll_offset > max_offset {
            self.scroll_offset = max_offset;
        }

        let block = Block::default().borders(Borders::ALL).title(tab_title_line(
            active,
            &self.bee_buffers,
            self.scroll_offset,
            self.h_scroll_offset,
            filter_active,
            match_count,
            t,
        ));
        let inner = block.inner(area);
        frame.render_widget(block, area);

        // When the in-pane filter prompt is open, eat the top row
        // of the content area for the live-typed input. The match
        // count updates per keystroke from the buffer (not the
        // committed filter), so operators see whether their typing
        // will hit anything before they press Enter.
        let prompt_open = self.filter_prompt.is_some();
        let chunks = if prompt_open {
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(inner)
        } else {
            Layout::vertical([Constraint::Min(0)]).split(inner)
        };
        if prompt_open {
            let buf = self.filter_prompt_buffer().to_string();
            // Compute the "preview" match count for the *current*
            // buffer (independent of the committed filter) so the
            // operator sees match-count update as they type.
            let preview_lines: Vec<Line> = match active {
                LogTab::SelfHttp => render_self_http(&self.self_http_entries, t),
                LogTab::Cockpit => render_cockpit(&self.cockpit_entries, t),
                tab => render_bee_tab(
                    &self.bee_buffers,
                    tab,
                    self.spawn_active,
                    self.log_source_hint.as_deref(),
                    t,
                ),
            };
            let preview_matches = if buf.trim().is_empty() {
                preview_lines.len()
            } else {
                filter_lines(preview_lines, &buf).len()
            };
            let prompt_line = Line::from(vec![
                Span::styled(
                    "  /",
                    Style::default().fg(t.accent).add_modifier(Modifier::BOLD),
                ),
                Span::styled(buf, Style::default().fg(t.info)),
                Span::styled("_", Style::default().fg(t.dim)),
                Span::raw("   "),
                Span::styled(
                    format!("{preview_matches} matches · Enter commits · Esc cancels"),
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ),
            ]);
            frame.render_widget(Paragraph::new(prompt_line), chunks[0]);
        }
        let content_area = if prompt_open { chunks[1] } else { chunks[0] };

        // Pick the visible window: tail semantics + scroll offset.
        // - tailing (offset = 0): show the last `render_h` lines.
        // - scrolled back: show [end-render_h .. end), where
        //   end = total - offset.
        let render_h = content_area.height as usize;
        let visible: Vec<Line> = if lines.len() > render_h {
            let end = lines.len().saturating_sub(self.scroll_offset);
            let start = end.saturating_sub(render_h);
            lines.into_iter().skip(start).take(end - start).collect()
        } else {
            lines
        };

        // Vertical position is already encoded by the visible
        // window slice; use ratatui's scroll() for the horizontal
        // axis only. (Mixing scroll() with our slice-windowed
        // visible vec works because the slice is what we want to
        // render — scroll() merely shifts each rendered line left
        // by `h_scroll_offset` columns.)
        frame.render_widget(
            Paragraph::new(visible).scroll((0, self.h_scroll_offset)),
            content_area,
        );
        Ok(())
    }
}

/// Case-insensitive substring filter on a vector of ratatui `Line`s.
/// Each `Line` is reduced to its concatenated text via the existing
/// `to_string()` impl and compared against the lowercased query.
/// Pure for testability — `tests/s10_log_filter.rs` (and the unit
/// tests below) exercise this without spinning a TUI.
pub fn filter_lines<'a>(lines: Vec<Line<'a>>, query: &str) -> Vec<Line<'a>> {
    let needle = query.to_lowercase();
    if needle.is_empty() {
        return lines;
    }
    lines
        .into_iter()
        .filter(|line| line.to_string().to_lowercase().contains(&needle))
        .collect()
}

impl LogPane {
    /// Number of payload lines the active tab currently has. Used
    /// by `draw()` to clamp the scroll offset and by tests.
    pub fn active_tab_total_lines(&self) -> usize {
        match self.active_tab {
            LogTab::SelfHttp => self.self_http_entries.len(),
            LogTab::Cockpit => self.cockpit_entries.len(),
            tab => self.bee_buffers.count(tab),
        }
    }
}

/// Build the `[Errors 3] [Warn 0] [Info 247] [Debug 1.2k] [Bee HTTP] [bee::http]`
/// title strip with the active tab highlighted and counters from the
/// per-tab buffers. Counters above 999 collapse to `1.2k` style so
/// the strip fits an 80-column terminal.
fn tab_title_line<'a>(
    active: LogTab,
    bufs: &BeeLogBuffers,
    scroll_offset: usize,
    h_scroll_offset: u16,
    filter: Option<&str>,
    filter_match_count: Option<usize>,
    t: &theme::Theme,
) -> Line<'a> {
    let mut spans: Vec<Span> = Vec::with_capacity(LogTab::ALL.len() * 2 + 2);
    spans.push(Span::raw(" "));
    for tab in LogTab::ALL {
        let count = bufs.count(tab);
        // SelfHttp + Cockpit don't have BeeLogBuffer counts (their
        // payload comes from the in-process capture buffers, not the
        // supervisor's tail), so render the label without a count.
        let label = if count == 0 || matches!(tab, LogTab::SelfHttp | LogTab::Cockpit) {
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
    // "paused N ↑" indicator when the operator has scrolled back.
    // Bright warn-yellow so it's impossible to miss that the pane
    // is no longer auto-tailing.
    if scroll_offset > 0 {
        spans.push(Span::styled(
            format!(" paused {scroll_offset} ↑ "),
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ));
    }
    if h_scroll_offset > 0 {
        // Sibling indicator to "paused N ↑" — surfaces horizontal
        // pan state so an operator who walked away and came back
        // sees why their log lines look chopped on the left.
        spans.push(Span::styled(
            format!(" → {h_scroll_offset} "),
            Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(q) = filter {
        // Active filter chip with the live query + match count so
        // operators can confirm the filter is doing what they
        // expect without flipping tabs.
        let count = filter_match_count.unwrap_or(0);
        spans.push(Span::styled(
            format!(" /{q} · {count} matches "),
            Style::default().fg(t.info).add_modifier(Modifier::BOLD),
        ));
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
        LogTab::Cockpit => Style::default().fg(t.accent),
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

fn render_cockpit<'a>(entries: &'a [CockpitEntry], t: &theme::Theme) -> Vec<Line<'a>> {
    if entries.is_empty() {
        return vec![Line::from(Span::styled(
            "  (no cockpit-internal events captured yet)",
            Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
        ))];
    }
    entries.iter().map(|e| cockpit_line(e, t)).collect()
}

fn cockpit_line<'a>(e: &'a CockpitEntry, t: &theme::Theme) -> Line<'a> {
    let level_style = match e.level.as_str() {
        "ERROR" => Style::default().fg(t.fail).add_modifier(Modifier::BOLD),
        "WARN" => Style::default().fg(t.warn).add_modifier(Modifier::BOLD),
        "INFO" => Style::default().fg(t.info),
        _ => Style::default().fg(t.dim),
    };
    Line::from(vec![
        Span::styled(format!("{} ", e.ts), Style::default().fg(t.dim)),
        Span::styled(format!("{:<5}", e.level), level_style),
        Span::raw(" "),
        Span::styled(
            format!("{:<22}", trim_target(&e.target)),
            Style::default().fg(t.accent),
        ),
        Span::raw("  "),
        Span::raw(e.message.clone()),
    ])
}

/// Drop the leading `bee_tui::` prefix on cockpit-event targets so
/// the rendered line stays under 80 columns. `bee_tui::watch::peers`
/// → `watch::peers`.
fn trim_target(target: &str) -> &str {
    target.strip_prefix("bee_tui::").unwrap_or(target)
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
    log_source_hint: Option<&str>,
    t: &theme::Theme,
) -> Vec<Line<'a>> {
    let buf = match bufs.buffer_for(tab) {
        Some(b) => b,
        None => return Vec::new(),
    };
    if buf.is_empty() {
        let dim = Style::default().fg(t.dim).add_modifier(Modifier::ITALIC);
        if spawn_active {
            return vec![Line::from(Span::styled(
                "  (awaiting bee log entries on this severity…)",
                dim,
            ))];
        }
        if let Some(hint) = log_source_hint {
            // Auto-discovery found a local Bee it can't tail — show
            // the explanation, one sentence per line so it stays
            // readable in the narrow pane.
            let head = Style::default().fg(t.warn).add_modifier(Modifier::BOLD);
            let mut lines = vec![Line::from(Span::styled(
                "  log auto-discovery — can't capture this Bee's log:",
                head,
            ))];
            for sentence in hint.split_inclusive(". ") {
                lines.push(Line::from(Span::styled(
                    format!("  {}", sentence.trim()),
                    dim,
                )));
            }
            return lines;
        }
        return vec![Line::from(Span::styled(
            "  (no bee log source — spawn Bee via [bee] / --bee-bin, or tail an \
             external Bee with [[nodes]].log_file / log_command)",
            dim,
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
            LogTab::Cockpit,
            LogTab::Errors,
        ] {
            assert_eq!(pane.next_tab(), expected);
        }
    }

    #[test]
    fn prev_tab_wraps() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        for expected in [
            LogTab::Cockpit,
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
    fn fresh_pane_is_tailing() {
        let pane = LogPane::new(None, LogTab::Errors, 10);
        assert!(pane.is_tailing());
        assert_eq!(pane.scroll_offset(), 0);
    }

    #[test]
    fn scroll_up_disables_tail_and_remembers_offset() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.scroll_up(3);
        assert!(!pane.is_tailing());
        assert_eq!(pane.scroll_offset(), 3);
        pane.scroll_up(2);
        assert_eq!(pane.scroll_offset(), 5);
    }

    #[test]
    fn scroll_down_eventually_resumes_tail() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.scroll_up(5);
        pane.scroll_down(2);
        assert_eq!(pane.scroll_offset(), 3);
        // Saturating-sub: scrolling down past 0 snaps to tail.
        pane.scroll_down(100);
        assert_eq!(pane.scroll_offset(), 0);
        assert!(pane.is_tailing());
    }

    #[test]
    fn resume_tail_resets_offset() {
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.scroll_up(7);
        pane.resume_tail();
        assert!(pane.is_tailing());
    }

    #[test]
    fn tab_switch_resets_scroll_offset() {
        // A scroll offset on tab A makes no sense on tab B — different
        // ring buffer, different content. Reset on switch.
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.scroll_up(4);
        pane.next_tab();
        assert_eq!(pane.scroll_offset(), 0);
        assert!(pane.is_tailing());
        // Same on prev_tab.
        pane.scroll_up(4);
        pane.prev_tab();
        assert_eq!(pane.scroll_offset(), 0);
    }

    #[test]
    fn push_bee_bumps_offset_for_active_tab_when_scrolled() {
        // Scroll-back stability: when the operator is scrolled up
        // and a new entry lands on the same tab, the offset bumps
        // so the visible window stays anchored on the same content.
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.push_bee(LogTab::Errors, line("err1"));
        pane.push_bee(LogTab::Errors, line("err2"));
        pane.scroll_up(2);
        pane.push_bee(LogTab::Errors, line("err3"));
        // Offset went from 2 → 3 to compensate for the new entry
        // shifting the window's relative position.
        assert_eq!(pane.scroll_offset(), 3);
    }

    #[test]
    fn push_bee_doesnt_bump_offset_when_tailing() {
        // While tailing (offset = 0) the pane should keep tailing
        // without spuriously paging into "paused" mode.
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        for i in 0..5 {
            pane.push_bee(LogTab::Errors, line(&format!("e{i}")));
        }
        assert_eq!(pane.scroll_offset(), 0);
        assert!(pane.is_tailing());
    }

    #[test]
    fn push_bee_doesnt_bump_offset_for_inactive_tab() {
        // Activity on a different tab shouldn't move the operator's
        // anchor on the one they're reading.
        let mut pane = LogPane::new(None, LogTab::Errors, 10);
        pane.push_bee(LogTab::Errors, line("err1"));
        pane.scroll_up(1);
        let before = pane.scroll_offset();
        pane.push_bee(LogTab::Debug, line("dbg1"));
        assert_eq!(pane.scroll_offset(), before);
    }

    fn line(msg: &str) -> BeeLogLine {
        BeeLogLine {
            timestamp: "t".into(),
            logger: "node/test".into(),
            message: msg.into(),
        }
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

    #[test]
    fn h_scroll_starts_at_zero() {
        let pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT);
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    #[test]
    fn scroll_right_then_left_returns_to_zero() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT);
        pane.scroll_right(8);
        pane.scroll_right(8);
        assert_eq!(pane.h_scroll_offset(), 16);
        pane.scroll_left(16);
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    #[test]
    fn scroll_left_saturates_at_zero() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT);
        pane.scroll_left(100);
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    #[test]
    fn switching_tabs_resets_h_scroll() {
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        pane.scroll_right(40);
        assert_eq!(pane.h_scroll_offset(), 40);
        pane.next_tab();
        assert_eq!(pane.h_scroll_offset(), 0);
        pane.scroll_right(20);
        pane.prev_tab();
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    #[test]
    fn resume_tail_resets_both_axes() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT);
        pane.scroll_up(5);
        pane.scroll_right(24);
        pane.resume_tail();
        assert_eq!(pane.scroll_offset(), 0);
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    #[test]
    fn reset_h_scroll_only_touches_horizontal() {
        let mut pane = LogPane::new(None, LogTab::SelfHttp, LOG_PANE_MIN_HEIGHT);
        pane.scroll_up(7);
        pane.scroll_right(16);
        pane.reset_h_scroll();
        assert_eq!(pane.scroll_offset(), 7);
        assert_eq!(pane.h_scroll_offset(), 0);
    }

    // --- v1.13.0 filter tests ---

    fn mk_line(s: &str) -> Line<'static> {
        Line::from(Span::raw(s.to_string()))
    }

    #[test]
    fn filter_lines_empty_query_passes_through() {
        let lines = vec![mk_line("alpha"), mk_line("beta"), mk_line("gamma")];
        let out = filter_lines(lines.clone(), "");
        assert_eq!(out.len(), lines.len());
    }

    #[test]
    fn filter_lines_matches_case_insensitive_substring() {
        let lines = vec![
            mk_line("GET /status 503 Node is syncing"),
            mk_line("GET /health 200 4ms"),
            mk_line("GET /STATUS 503 Node is syncing"),
        ];
        let out = filter_lines(lines, "status");
        // Matches both /status and /STATUS (case-insensitive).
        assert_eq!(out.len(), 2);
    }

    #[test]
    fn filter_lines_drops_non_matching() {
        let lines = vec![mk_line("foo"), mk_line("bar"), mk_line("foobar")];
        let out = filter_lines(lines, "foo");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].to_string(), "foo");
        assert_eq!(out[1].to_string(), "foobar");
    }

    #[test]
    fn filter_prompt_lifecycle_opens_commits_and_clears() {
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        assert!(!pane.filter_prompt_visible());
        assert!(pane.active_filter().is_none());
        pane.open_filter_prompt();
        assert!(pane.filter_prompt_visible());
        pane.push_filter_char('5');
        pane.push_filter_char('0');
        pane.push_filter_char('3');
        assert_eq!(pane.filter_prompt_buffer(), "503");
        pane.commit_filter_prompt();
        assert!(!pane.filter_prompt_visible());
        assert_eq!(pane.active_filter(), Some("503"));
        pane.clear_filter();
        assert!(pane.active_filter().is_none());
    }

    #[test]
    fn filter_prompt_empty_commit_clears_existing_filter() {
        // Operator opens the prompt, deletes everything, and
        // presses Enter — that should clear the filter (saves a
        // separate "clear" keystroke).
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        pane.open_filter_prompt();
        pane.push_filter_char('x');
        pane.commit_filter_prompt();
        assert_eq!(pane.active_filter(), Some("x"));
        pane.open_filter_prompt();
        pane.pop_filter_char();
        pane.commit_filter_prompt();
        assert!(pane.active_filter().is_none());
    }

    #[test]
    fn filter_prompt_cancel_preserves_active_filter() {
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        pane.open_filter_prompt();
        pane.push_filter_char('a');
        pane.commit_filter_prompt();
        // Now reopen the prompt, type something different, then
        // cancel. The previous filter should remain in effect.
        pane.open_filter_prompt();
        pane.push_filter_char('z');
        pane.cancel_filter_prompt();
        assert_eq!(pane.active_filter(), Some("a"));
    }

    #[test]
    fn open_filter_prompt_seeds_with_active_filter() {
        // So editing an existing filter doesn't require retyping.
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        pane.open_filter_prompt();
        pane.push_filter_char('a');
        pane.push_filter_char('b');
        pane.commit_filter_prompt();
        pane.open_filter_prompt();
        assert_eq!(pane.filter_prompt_buffer(), "ab");
    }

    #[test]
    fn filter_commit_resets_scroll_offset() {
        // A new filter changes which lines are visible; auto-tail
        // to the bottom of the filtered set so the operator sees
        // the most recent matches.
        let mut pane = LogPane::new(None, LogTab::Errors, LOG_PANE_MIN_HEIGHT);
        pane.scroll_up(20);
        assert_eq!(pane.scroll_offset(), 20);
        pane.open_filter_prompt();
        pane.push_filter_char('x');
        pane.commit_filter_prompt();
        assert_eq!(pane.scroll_offset(), 0);
    }
}
