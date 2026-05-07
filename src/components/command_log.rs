//! S10 — Command-log pane (`docs/PLAN.md` § 8.S10).
//!
//! Subscribes to the process-wide [`crate::log_capture::LogCapture`]
//! installed by [`crate::logging::init`] and renders the most recent
//! `bee::http` events in a lazygit-style append-only tail. This is
//! the cockpit's trust anchor and live tutorial — operators see the
//! actual HTTP request being made for every gauge they're watching.

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};

use super::Component;
use crate::action::Action;
use crate::log_capture::{LogCapture, LogEntry};
use crate::theme;

/// S10 component. Cheap to construct — the actual buffer is the
/// shared [`LogCapture`] handle.
pub struct CommandLog {
    capture: Option<LogCapture>,
    entries: Vec<LogEntry>,
}

impl CommandLog {
    pub fn new(capture: Option<LogCapture>) -> Self {
        Self {
            capture,
            entries: Vec::new(),
        }
    }

    fn pull_latest(&mut self) {
        if let Some(c) = &self.capture {
            self.entries = c.snapshot();
        }
    }
}

impl Component for CommandLog {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        // Render the last (area.height - 2) entries — minus 2 for
        // the top + bottom borders of the framing block. Newest at
        // the bottom (append-only tail).
        let inner_h = area.height.saturating_sub(2) as usize;
        let total = self.entries.len();
        let start = total.saturating_sub(inner_h);
        let t = theme::active();
        let lines: Vec<Line> = self.entries[start..]
            .iter()
            .map(|e| {
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
            })
            .collect();

        let block = Block::default().borders(Borders::ALL).title(Span::styled(
            " bee::http ",
            Style::default()
                .fg(t.accent)
                .add_modifier(Modifier::BOLD),
        ));

        let widget = if lines.is_empty() {
            Paragraph::new(Line::from(Span::styled(
                "  (waiting for first request…)",
                Style::default()
                    .fg(t.dim)
                    .add_modifier(Modifier::ITALIC),
            )))
            .block(block)
        } else {
            Paragraph::new(lines).block(block)
        };
        frame.render_widget(widget, area);
        Ok(())
    }
}

/// Per-method colour, lazygit-style.
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
    fn path_only_strips_scheme_and_host() {
        assert_eq!(
            path_only("http://localhost:1633/status"),
            "/status".to_string()
        );
        assert_eq!(
            path_only("https://bee.example.com:1633/stamps/abc"),
            "/stamps/abc".to_string()
        );
    }

    #[test]
    fn path_only_handles_root_only() {
        // No path component after host — return the URL unchanged.
        assert_eq!(path_only("http://localhost:1633"), "http://localhost:1633");
    }
}
