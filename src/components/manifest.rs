//! S12 — Manifests screen (Mantaray tree browser). Pure view-data
//! half lives in [`bee_cockpit_core::views::manifest`]; this module
//! owns the async-fetch channels, the selection cursor, the scroll
//! offset, and the ratatui draw / key path.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bee::manifest::MantarayNode;
use bee::swarm::Reference;
use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::mpsc;

pub use bee_cockpit_core::views::manifest::{
    ManifestView, NodeState, TreeRow, hex_lower, parse_hex_32, short_hex, view_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::manifest_walker;
use crate::theme;

type FetchResult = (FetchTarget, std::result::Result<MantarayNode, String>);

#[derive(Debug, Clone)]
enum FetchTarget {
    Root(Reference),
    Fork([u8; 32]),
}

pub struct Manifest {
    api: Arc<ApiClient>,
    /// Set when `:manifest <ref>` fires; cleared on `:manifest` with
    /// no arg (or by typing a different ref over the existing one).
    root_ref: Option<Reference>,
    root: NodeState,
    /// Per-self-address load states for child fork nodes.
    forks_loaded: HashMap<[u8; 32], NodeState>,
    /// Self-addresses of forks that are currently expanded in the UI.
    expanded: HashSet<[u8; 32]>,
    selected: usize,
    scroll_offset: usize,
    fetch_tx: mpsc::UnboundedSender<FetchResult>,
    fetch_rx: mpsc::UnboundedReceiver<FetchResult>,
}

impl Manifest {
    pub fn new(api: Arc<ApiClient>) -> Self {
        let (fetch_tx, fetch_rx) = mpsc::unbounded_channel();
        Self {
            api,
            root_ref: None,
            root: NodeState::Idle,
            forks_loaded: HashMap::new(),
            expanded: HashSet::new(),
            selected: 0,
            scroll_offset: 0,
            fetch_tx,
            fetch_rx,
        }
    }

    /// Kick off a root-chunk fetch for `reference`. Replaces any
    /// in-flight or completed manifest.
    pub fn load(&mut self, reference: Reference) {
        self.root_ref = Some(reference.clone());
        self.root = NodeState::Loading;
        self.forks_loaded.clear();
        self.expanded.clear();
        self.selected = 0;
        self.scroll_offset = 0;
        let api = self.api.clone();
        let tx = self.fetch_tx.clone();
        let target_ref = reference.clone();
        tokio::spawn(async move {
            let r = manifest_walker::load_node(api, target_ref.clone()).await;
            let _ = tx.send((FetchTarget::Root(target_ref), r));
        });
    }

    /// Re-export of core's pure view computation, kept as an inherent
    /// function so existing `Manifest::view_for` call sites resolve.
    pub fn view_for(
        root_ref: Option<&Reference>,
        root: &NodeState,
        forks_loaded: &HashMap<[u8; 32], NodeState>,
        expanded: &HashSet<[u8; 32]>,
    ) -> ManifestView {
        view_for(root_ref, root, forks_loaded, expanded)
    }

    fn drain_fetches(&mut self) {
        while let Ok((target, result)) = self.fetch_rx.try_recv() {
            let state = match result {
                Ok(node) => NodeState::Loaded(Box::new(node)),
                Err(e) => NodeState::Error(e),
            };
            match target {
                FetchTarget::Root(r) => {
                    if Some(r) == self.root_ref {
                        self.root = state;
                    }
                }
                FetchTarget::Fork(addr) => {
                    self.forks_loaded.insert(addr, state);
                }
            }
        }
    }

    fn cached_view(&self) -> ManifestView {
        view_for(
            self.root_ref.as_ref(),
            &self.root,
            &self.forks_loaded,
            &self.expanded,
        )
    }

    fn select_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    fn select_down(&mut self) {
        let view = self.cached_view();
        if !view.rows.is_empty() && self.selected + 1 < view.rows.len() {
            self.selected += 1;
        }
    }

    /// Toggle expand on the highlighted row. If the fork's child is
    /// not yet loaded, kick off an async fetch instead.
    fn toggle_selected(&mut self) {
        let view = self.cached_view();
        if view.rows.is_empty() {
            return;
        }
        let row = &view.rows[self.selected.min(view.rows.len() - 1)];
        let Some(ref hex) = row.self_addr_hex else {
            return;
        };
        let Ok(addr) = parse_hex_32(hex) else {
            return;
        };
        if !row.has_children {
            return;
        }
        if self.expanded.contains(&addr) {
            self.expanded.remove(&addr);
            return;
        }
        if matches!(self.forks_loaded.get(&addr), Some(NodeState::Loaded(_))) {
            self.expanded.insert(addr);
            return;
        }
        self.forks_loaded.insert(addr, NodeState::Loading);
        let api = self.api.clone();
        let tx = self.fetch_tx.clone();
        tokio::spawn(async move {
            let reference = match Reference::new(&addr) {
                Ok(r) => r,
                Err(e) => {
                    let _ = tx.send((
                        FetchTarget::Fork(addr),
                        Err(format!("invalid child reference: {e}")),
                    ));
                    return;
                }
            };
            let r = manifest_walker::load_node(api, reference).await;
            let _ = tx.send((FetchTarget::Fork(addr), r));
        });
        self.expanded.insert(addr);
    }
}

impl Component for Manifest {
    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.drain_fetches();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        match key.code {
            KeyCode::Up | KeyCode::Char('k') => self.select_up(),
            KeyCode::Down | KeyCode::Char('j') => self.select_down(),
            KeyCode::Enter => self.toggle_selected(),
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let t = theme::active();
        let view = self.cached_view();
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        let header_text = if view.rows.is_empty() {
            view.header.clone()
        } else {
            format!(
                "{}\n  {}",
                view.header,
                view.root_ref_hex.clone().unwrap_or_default()
            )
        };
        frame.render_widget(
            Paragraph::new(header_text).block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() + 1);
        if view.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no manifest loaded — type `:manifest <ref>` or `:inspect <ref>`)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            if self.selected >= view.rows.len() {
                self.selected = view.rows.len() - 1;
            }
            let body_h = chunks[1].height as usize;
            if self.selected < self.scroll_offset {
                self.scroll_offset = self.selected;
            } else if self.selected >= self.scroll_offset + body_h.max(1) {
                self.scroll_offset = self.selected + 1 - body_h.max(1);
            }

            for (i, row) in view.rows.iter().enumerate() {
                if i < self.scroll_offset {
                    continue;
                }
                let is_cursor = i == self.selected;
                let indent: String = "  ".repeat(row.depth as usize + 1);
                let label_style = if is_cursor {
                    Style::default().bg(t.tab_active_bg).fg(t.tab_active_fg)
                } else {
                    Style::default()
                };
                let cursor_marker = if is_cursor { "▸ " } else { "  " };
                let mut spans = vec![
                    Span::styled(cursor_marker.to_string(), Style::default().fg(t.accent)),
                    Span::raw(indent),
                    Span::styled(row.glyph.to_string(), Style::default().fg(t.accent)),
                    Span::raw(" "),
                    Span::styled(row.label.clone(), label_style),
                ];
                if let Some(ct) = &row.content_type {
                    spans.push(Span::styled(
                        format!("  [{ct}]"),
                        Style::default().fg(t.info),
                    ));
                }
                if let Some(ref_hex) = &row.target_ref_hex {
                    spans.push(Span::styled(
                        format!("  → {}", short_hex(ref_hex, 8)),
                        Style::default().fg(t.dim),
                    ));
                }
                if let Some(hint) = &row.state_hint {
                    spans.push(Span::styled(
                        format!("  ({hint})"),
                        Style::default().fg(t.warn).add_modifier(Modifier::ITALIC),
                    ));
                }
                lines.push(Line::from(spans));
            }
        }
        frame.render_widget(Paragraph::new(lines), chunks[1]);

        if !view.rows.is_empty() {
            let row = &view.rows[self.selected.min(view.rows.len() - 1)];
            let detail = match (&row.target_ref_hex, &row.self_addr_hex) {
                (Some(t_ref), _) => format!("  selected: target {t_ref}"),
                (None, Some(s)) => format!("  selected: chunk {s}"),
                _ => "  (no copyable id on this row)".to_string(),
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(detail, Style::default().fg(t.dim)))),
                chunks[2],
            );
        }

        let footer = Line::from(vec![
            Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" switch screen  "),
            Span::styled(
                " ↑↓/jk ",
                Style::default().fg(Color::Black).bg(Color::White),
            ),
            Span::raw(" select  "),
            Span::styled(" ↵ ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" expand/collapse  "),
            Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" help  "),
            Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
            Span::raw(" quit "),
        ]);
        frame.render_widget(Paragraph::new(footer), chunks[3]);
        Ok(())
    }
}
