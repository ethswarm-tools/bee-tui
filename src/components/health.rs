//! S1 — Health gates screen (`docs/PLAN.md` § 8.S1). Pure view-data
//! half lives in [`bee_cockpit_core::views::health`]; this module
//! owns the API client handle, the watch subscriptions, the scroll
//! cursor, and the ratatui draw path.

use std::sync::Arc;

use color_eyre::Result;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

pub use bee_cockpit_core::views::health::{
    Gate, GateStatus, gates_for, gates_for_with_stamps,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::{HealthSnapshot, StampsSnapshot, TopologySnapshot};

fn gate_glyph(s: GateStatus) -> &'static str {
    let g = theme::active().glyphs;
    match s {
        GateStatus::Pass => g.pass,
        GateStatus::Warn => g.warn,
        GateStatus::Fail => g.fail,
        GateStatus::Unknown => g.bullet,
    }
}

fn gate_color(s: GateStatus) -> Color {
    let t = theme::active();
    match s {
        GateStatus::Pass => t.pass,
        GateStatus::Warn => t.warn,
        GateStatus::Fail => t.fail,
        GateStatus::Unknown => t.dim,
    }
}

/// S1 component. Subscribes to the [`HealthSnapshot`] watch channel
/// from the [`crate::watch::BeeWatch`] hub plus the [`TopologySnapshot`]
/// stream that drives the bin-saturation gate.
pub struct Health {
    api: Arc<ApiClient>,
    rx: watch::Receiver<HealthSnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    snapshot: HealthSnapshot,
    topology: TopologySnapshot,
    /// Free-scroll offset for the gates list — 10 gates with their
    /// `why` lines overflow a short terminal.
    scroll_offset: usize,
}

impl Health {
    pub fn new(
        api: Arc<ApiClient>,
        rx: watch::Receiver<HealthSnapshot>,
        topology_rx: watch::Receiver<TopologySnapshot>,
    ) -> Self {
        let snapshot = rx.borrow().clone();
        let topology = topology_rx.borrow().clone();
        Self {
            api,
            rx,
            topology_rx,
            snapshot,
            topology,
            scroll_offset: 0,
        }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
    }

    /// Re-export of core's pure gate computation, kept as an inherent
    /// function so existing `Health::gates_for` call sites resolve.
    pub fn gates_for(snap: &HealthSnapshot, topology: Option<&TopologySnapshot>) -> Vec<Gate> {
        gates_for(snap, topology)
    }

    /// Re-export of core's stamps-aware gate computation, kept as
    /// an inherent function so the alerts pipeline + `:diagnose`
    /// bundle continue to resolve `Health::gates_for_with_stamps`.
    pub fn gates_for_with_stamps(
        snap: &HealthSnapshot,
        topology: Option<&TopologySnapshot>,
        stamps: Option<&StampsSnapshot>,
    ) -> Vec<Gate> {
        gates_for_with_stamps(snap, topology, stamps)
    }
}

impl Component for Health {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: crossterm::event::KeyEvent) -> Result<Option<Action>> {
        self.scroll_offset = super::scroll::scroll_key(self.scroll_offset, key.code);
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

        let header_line1 = Line::from(vec![
            Span::styled("HEALTH", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                format!("{} · {}", self.api.name, self.api.url),
                Style::default().fg(theme::active().info),
            ),
            Span::raw(if self.api.authenticated { "  🔒" } else { "" }),
        ]);
        let mut header_line2 = vec![Span::raw("ping: ")];
        let t = theme::active();
        match self.snapshot.last_ping {
            Some(d) => header_line2.push(Span::styled(
                format!("{}ms", d.as_millis()),
                Style::default().fg(t.pass),
            )),
            None => header_line2.push(Span::styled("—", Style::default().fg(t.dim))),
        };
        if let Some(err) = &self.snapshot.last_error {
            header_line2.push(Span::raw("  "));
            let (color, msg) = theme::classify_header_error(err);
            header_line2.push(Span::styled(msg, Style::default().fg(color)));
        }
        frame.render_widget(
            Paragraph::new(vec![header_line1, Line::from(header_line2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let mut lines: Vec<Line> = Vec::new();
        for g in gates_for(&self.snapshot, Some(&self.topology)) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    gate_glyph(g.status),
                    Style::default()
                        .fg(gate_color(g.status))
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw("  "),
                Span::styled(
                    format!("{:<28}", g.label),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::raw(g.value),
            ]));
            if let Some(why) = g.why {
                lines.push(Line::from(vec![
                    Span::raw("       └─ "),
                    Span::styled(
                        why,
                        Style::default()
                            .fg(theme::active().dim)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
        let gates_area = chunks[1];
        let visible_rows = gates_area.height as usize;
        self.scroll_offset =
            super::scroll::clamp_offset(self.scroll_offset, visible_rows, lines.len());
        let total = lines.len();
        frame.render_widget(
            Paragraph::new(lines).scroll((self.scroll_offset as u16, 0)),
            gates_area,
        );
        super::scroll::render_scrollbar(frame, gates_area, self.scroll_offset, visible_rows, total);

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" scroll  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
            ])),
            chunks[2],
        );

        Ok(())
    }
}
