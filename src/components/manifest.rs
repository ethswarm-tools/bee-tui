//! S12 — Manifests screen (Mantaray tree browser).
//!
//! The first screen in bee-tui that gives operators X-ray vision into
//! their *data* (not just their node). `:manifest <ref>` loads a
//! Swarm reference's chunk; if it parses as a Mantaray manifest, the
//! tree is opened here. `:inspect <ref>` is the universal "what is
//! this thing?" verb that routes manifests to S12 and prints the
//! shape of raw chunks.
//!
//! ## Render path
//!
//! Pure [`Manifest::view_for`] turns `(root, loaded, expanded)` into
//! a flat [`ManifestView`] of [`TreeRow`]s. Snapshot tests in
//! `tests/s12_manifest_view.rs` lock the rendering against future
//! refactors.
//!
//! ## Tree state machine
//!
//! - `root: NodeState` — the root chunk's load status.
//! - `forks_loaded: HashMap<[u8;32], NodeState>` — child fork nodes,
//!   keyed by `self_address`.
//! - `expanded: HashSet<[u8;32]>` — which fork-self-addresses are
//!   currently shown expanded in the visible tree.
//!
//! Pressing Enter on a row that has children either toggles its
//! expanded flag (cheap) or starts an async fetch (when the child
//! isn't loaded yet).
//!
//! ## What's intentionally out of scope (v1)
//!
//! - **Encrypted refs (64 bytes).** v1.2 returns an error; once the
//!   recursive walk handles obfuscation-key derivation we can lift it.
//! - **Manifest editing / re-uploading.** Read-only browser only.
//! - **Path-based addressing.** Operator types a chunk reference, not
//!   `<ref>/path/in/manifest`. Path-resolve lives in bee-rs's
//!   `resolve_path`; a v1.4 follow-up can pre-walk to a specified path.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use bee::manifest::{MantarayNode, TYPE_EDGE, TYPE_VALUE};
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

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::manifest_walker;
use crate::theme;

/// Per-node load status. Distinguishes "operator hasn't asked"
/// (`Idle`) from "we're fetching" (`Loading`) from "we have it"
/// (`Loaded`) from "fetch or parse failed" (`Error`).
#[derive(Debug, Clone)]
pub enum NodeState {
    Idle,
    Loading,
    Loaded(Box<MantarayNode>),
    Error(String),
}

impl NodeState {
    fn loaded(&self) -> Option<&MantarayNode> {
        match self {
            Self::Loaded(n) => Some(n),
            _ => None,
        }
    }
}

/// One row in the flat-rendered tree. Pure data; the renderer just
/// walks `ManifestView::rows`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    /// Indentation level (0 = root's direct children).
    pub depth: u8,
    /// Path segment for this fork (UTF-8-decoded prefix bytes if
    /// valid, else hex-escaped). `(root)` for depth-0 root summary.
    pub label: String,
    /// Glyph that signals state at a glance:
    /// `▼` expanded, `▶` collapsed-with-children, `·` leaf-only-target,
    /// `⌛` loading, `✗` error.
    pub glyph: char,
    /// True when this fork has child forks (TYPE_EDGE bit set).
    pub has_children: bool,
    /// Self-address of this fork's node, hex-encoded. Used to key
    /// expand-state and load-state.
    pub self_addr_hex: Option<String>,
    /// File reference if this fork carries a target (TYPE_VALUE).
    pub target_ref_hex: Option<String>,
    /// Content-type from metadata, when present.
    pub content_type: Option<String>,
    /// Auxiliary status hint rendered in muted color
    /// (`loading…`, `error: …`).
    pub state_hint: Option<String>,
}

/// View fed to the renderer + snapshot tests. Pure transform of the
/// component's state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManifestView {
    /// Hex-encoded root reference, if `:manifest` has fired.
    pub root_ref_hex: Option<String>,
    /// One-line header summary: "32-byte chunk · 12 forks · 4 leaf nodes"
    /// or "no manifest loaded — type :manifest <ref>".
    pub header: String,
    /// Flat list of visible rows.
    pub rows: Vec<TreeRow>,
}

type FetchResult = (FetchTarget, Result<MantarayNode, String>);

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
    /// in-flight or completed manifest. Async; the result lands
    /// on the fetch channel and is drained on the next Tick.
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

    /// Build the visible-tree row list from current state.
    pub fn view_for(
        root_ref: Option<&Reference>,
        root: &NodeState,
        forks_loaded: &HashMap<[u8; 32], NodeState>,
        expanded: &HashSet<[u8; 32]>,
    ) -> ManifestView {
        let header = build_header(root_ref, root);
        let mut rows: Vec<TreeRow> = Vec::new();
        if let Some(node) = root.loaded() {
            walk_into_rows(node, 0, forks_loaded, expanded, &mut rows);
        }
        ManifestView {
            root_ref_hex: root_ref.map(|r| r.to_hex()),
            header,
            rows,
        }
    }

    fn cached_view(&self) -> ManifestView {
        Self::view_for(
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
        // Need to expand. Either we already have the child node or we
        // start a fetch.
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
        // Mark expanded immediately so the operator gets feedback;
        // the row will render `⌛ loading…` until the fetch lands.
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
        // 4-row split: header / body / detail / footer.
        let chunks = Layout::vertical([
            Constraint::Length(2),
            Constraint::Min(0),
            Constraint::Length(1),
            Constraint::Length(1),
        ])
        .split(area);

        // Header: chunk size + fork/leaf summary or load-state hint.
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

        // Body: tree rows.
        let mut lines: Vec<Line> = Vec::with_capacity(view.rows.len() + 1);
        if view.rows.is_empty() {
            lines.push(Line::from(Span::styled(
                "  (no manifest loaded — type `:manifest <ref>` or `:inspect <ref>`)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            // Clamp scroll/selected.
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

        // Detail: full ID under cursor for click-drag copy.
        if !view.rows.is_empty() {
            let row = &view.rows[self.selected.min(view.rows.len() - 1)];
            let detail = match (&row.target_ref_hex, &row.self_addr_hex) {
                (Some(t_ref), _) => format!("  selected: target {t_ref}"),
                (None, Some(s)) => format!("  selected: chunk {s}"),
                _ => "  (no copyable id on this row)".to_string(),
            };
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    detail,
                    Style::default().fg(t.dim),
                ))),
                chunks[2],
            );
        }

        // Footer keymap.
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

// ---- pure helpers ------------------------------------------------------

fn build_header(root_ref: Option<&Reference>, root: &NodeState) -> String {
    match (root_ref, root) {
        (None, _) => "no manifest loaded — type :manifest <ref> or :inspect <ref>".into(),
        (Some(r), NodeState::Idle) => format!("ref {} — pending", short_hex(&r.to_hex(), 8)),
        (Some(r), NodeState::Loading) => {
            format!("ref {} — loading root chunk…", short_hex(&r.to_hex(), 8))
        }
        (Some(r), NodeState::Error(e)) => {
            format!("ref {} — error: {}", short_hex(&r.to_hex(), 8), e)
        }
        (Some(r), NodeState::Loaded(node)) => {
            let fork_count = node.forks.len();
            let leaf_count = leaves_under(node);
            format!(
                "ref {} · {} fork{} · {} leaf node{}",
                short_hex(&r.to_hex(), 8),
                fork_count,
                if fork_count == 1 { "" } else { "s" },
                leaf_count,
                if leaf_count == 1 { "" } else { "s" }
            )
        }
    }
}

fn leaves_under(node: &MantarayNode) -> usize {
    if node.forks.is_empty() {
        // A node with no forks is itself a leaf if it has a target.
        return if node.is_null_target() { 0 } else { 1 };
    }
    node.forks
        .values()
        .map(|fork| {
            let t = fork.node.determine_type();
            (t & TYPE_VALUE != 0) as usize
        })
        .sum()
}

fn walk_into_rows(
    node: &MantarayNode,
    depth: u8,
    forks_loaded: &HashMap<[u8; 32], NodeState>,
    expanded: &HashSet<[u8; 32]>,
    rows: &mut Vec<TreeRow>,
) {
    for fork in node.forks.values() {
        let typ = fork.node.determine_type();
        let has_children = (typ & TYPE_EDGE) != 0;
        let has_target = (typ & TYPE_VALUE) != 0;
        let self_addr = fork.node.self_address;
        let target_ref_hex = if has_target && !fork.node.is_null_target() {
            Some(hex_lower(&fork.node.target_address))
        } else {
            None
        };
        let content_type = fork
            .node
            .metadata
            .as_ref()
            .and_then(|m| m.get("Content-Type").or_else(|| m.get("content-type")))
            .cloned();

        let is_expanded = self_addr
            .as_ref()
            .map(|a| expanded.contains(a))
            .unwrap_or(false);

        let load_state = self_addr.as_ref().and_then(|a| forks_loaded.get(a));
        let state_hint = match load_state {
            Some(NodeState::Loading) => Some("loading…".to_string()),
            Some(NodeState::Error(e)) => Some(format!("error: {e}")),
            _ => None,
        };
        let glyph = if state_hint
            .as_deref()
            .map(|s| s.starts_with("loading"))
            .unwrap_or(false)
        {
            '⌛'
        } else if state_hint
            .as_deref()
            .map(|s| s.starts_with("error"))
            .unwrap_or(false)
        {
            '✗'
        } else if has_children && is_expanded {
            '▼'
        } else if has_children {
            '▶'
        } else {
            '·'
        };

        rows.push(TreeRow {
            depth,
            label: prefix_to_label(&fork.prefix),
            glyph,
            has_children,
            self_addr_hex: self_addr.map(|a| hex_lower(&a)),
            target_ref_hex,
            content_type,
            state_hint,
        });

        // Recurse if expanded + we have the child loaded.
        if is_expanded {
            if let Some(addr) = self_addr {
                if let Some(NodeState::Loaded(child)) = forks_loaded.get(&addr) {
                    walk_into_rows(child, depth.saturating_add(1), forks_loaded, expanded, rows);
                }
            }
        }
    }
}

fn prefix_to_label(prefix: &[u8]) -> String {
    if prefix.is_empty() {
        return "(empty)".into();
    }
    if let Ok(s) = std::str::from_utf8(prefix) {
        if s.chars().all(|c| !c.is_control()) {
            return s.to_string();
        }
    }
    hex_lower(prefix)
}

fn hex_lower(b: &[u8]) -> String {
    let mut out = String::with_capacity(b.len() * 2);
    for byte in b {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn short_hex(s: &str, n: usize) -> String {
    if s.len() <= n * 2 + 1 {
        s.to_string()
    } else {
        format!("{}…{}", &s[..n], &s[s.len() - n..])
    }
}

fn parse_hex_32(s: &str) -> std::result::Result<[u8; 32], String> {
    let cleaned = s.trim().trim_start_matches("0x");
    if cleaned.len() != 64 {
        return Err(format!("expected 64 hex chars, got {}", cleaned.len()));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        out[i] = u8::from_str_radix(&cleaned[2 * i..2 * i + 2], 16)
            .map_err(|e| format!("hex: {e}"))?;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_state() -> (
        NodeState,
        HashMap<[u8; 32], NodeState>,
        HashSet<[u8; 32]>,
    ) {
        (NodeState::Idle, HashMap::new(), HashSet::new())
    }

    #[test]
    fn header_explains_no_load_yet() {
        let (root, loaded, expanded) = empty_state();
        let view = Manifest::view_for(None, &root, &loaded, &expanded);
        assert!(view.header.contains("no manifest loaded"), "{}", view.header);
        assert!(view.rows.is_empty());
    }

    #[test]
    fn header_explains_loading_state() {
        let (_, loaded, expanded) = empty_state();
        let root = NodeState::Loading;
        let r = Reference::from_hex(&"0".repeat(64)).unwrap();
        let view = Manifest::view_for(Some(&r), &root, &loaded, &expanded);
        assert!(view.header.contains("loading"), "{}", view.header);
        assert!(view.rows.is_empty());
    }

    #[test]
    fn header_propagates_load_error() {
        let (_, loaded, expanded) = empty_state();
        let root = NodeState::Error("download_chunk: 404".into());
        let r = Reference::from_hex(&"0".repeat(64)).unwrap();
        let view = Manifest::view_for(Some(&r), &root, &loaded, &expanded);
        assert!(view.header.contains("error"), "{}", view.header);
        assert!(view.header.contains("404"), "{}", view.header);
    }

    #[test]
    fn prefix_to_label_renders_utf8_when_possible() {
        assert_eq!(prefix_to_label(b"index.html"), "index.html");
        assert_eq!(prefix_to_label(&[]), "(empty)");
        // Control byte forces hex fallback.
        assert_eq!(prefix_to_label(&[0x00, 0x01, 0xff]), "0001ff");
    }

    #[test]
    fn short_hex_keeps_short_strings_intact() {
        assert_eq!(short_hex("abcd", 4), "abcd");
        let long = "a".repeat(64);
        let s = short_hex(&long, 8);
        assert!(s.contains('…'));
        assert_eq!(s.chars().filter(|c| *c == 'a').count(), 16);
    }

    #[test]
    fn parse_hex_32_round_trip() {
        let s = "ab".repeat(32);
        let arr = parse_hex_32(&s).unwrap();
        assert_eq!(arr[0], 0xab);
        assert_eq!(arr[31], 0xab);
        assert!(parse_hex_32(&"a".repeat(63)).is_err());
        assert!(parse_hex_32("0xABABA").is_err());
    }

    #[test]
    fn view_with_no_root_loaded_has_zero_rows() {
        let (root, loaded, expanded) = empty_state();
        let view = Manifest::view_for(None, &root, &loaded, &expanded);
        assert_eq!(view.rows.len(), 0);
        assert_eq!(view.root_ref_hex, None);
    }
}
