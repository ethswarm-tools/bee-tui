//! S1 — Health gates screen (`docs/PLAN.md` § 8.S1).
//!
//! Renders a vertical list of health gates derived from the latest
//! [`HealthSnapshot`]. Each gate carries a status (✓ / ⚠ / ✗ / ·),
//! a value line, and an optional `why` line that encodes the tribal
//! knowledge surfaced in `docs/research/05-operator-pain-points.md`
//! (e.g. "storageRadius decreases ONLY on the 30-min reserve worker
//! tick" — the #1 thing operators stare at and don't understand).

use std::sync::Arc;

use color_eyre::Result;
use num_bigint::BigInt;
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::watch;

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::watch::HealthSnapshot;

/// Tri-state outcome with an `Unknown` for "data not yet loaded".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateStatus {
    Pass,
    Warn,
    Fail,
    Unknown,
}

impl GateStatus {
    fn glyph(self) -> &'static str {
        match self {
            Self::Pass => "✓",
            Self::Warn => "⚠",
            Self::Fail => "✗",
            Self::Unknown => "·",
        }
    }
    fn color(self) -> Color {
        match self {
            Self::Pass => Color::Green,
            Self::Warn => Color::Yellow,
            Self::Fail => Color::Red,
            Self::Unknown => Color::DarkGray,
        }
    }
}

/// One row of the gates list.
#[derive(Debug, Clone)]
pub struct Gate {
    pub label: &'static str,
    pub status: GateStatus,
    pub value: String,
    /// Inline tooltip rendered as a dim italic continuation line. Used
    /// to encode tribal-knowledge hints (e.g. "wait for the next
    /// 30-min reserve worker tick").
    pub why: Option<String>,
}

/// S1 component. Subscribes to the [`HealthSnapshot`] watch channel
/// from the [`crate::watch::BeeWatch`] hub and renders a gate list.
pub struct Health {
    api: Arc<ApiClient>,
    rx: watch::Receiver<HealthSnapshot>,
    snapshot: HealthSnapshot,
}

impl Health {
    pub fn new(api: Arc<ApiClient>, rx: watch::Receiver<HealthSnapshot>) -> Self {
        let snapshot = rx.borrow().clone();
        Self { api, rx, snapshot }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
    }

    /// Pure, snapshot-driven gate computation. Exposed for snapshot
    /// tests so they can stub a [`HealthSnapshot`] and assert the
    /// resulting gate list without a running app loop.
    pub fn gates_for(snap: &HealthSnapshot) -> Vec<Gate> {
        let mut gates = Vec::with_capacity(10);

        // 1. API reachable -------------------------------------------------
        gates.push(match snap.last_ping {
            Some(d) => Gate {
                label: "API reachable",
                status: GateStatus::Pass,
                value: format!("({}ms)", d.as_millis()),
                why: None,
            },
            None if snap.last_update.is_none() => Gate {
                label: "API reachable",
                status: GateStatus::Unknown,
                value: "loading…".into(),
                why: None,
            },
            None => Gate {
                label: "API reachable",
                status: GateStatus::Fail,
                value: "no /health response".into(),
                why: snap.last_error.clone(),
            },
        });

        // 2. Chain RPC -----------------------------------------------------
        if let Some(cs) = &snap.chain_state {
            let delta = cs.chain_tip.saturating_sub(cs.block);
            let (status, why) = if delta == 0 {
                (GateStatus::Pass, None)
            } else if delta < 50 {
                (
                    GateStatus::Warn,
                    Some(format!("chain head {delta} blocks ahead")),
                )
            } else {
                (
                    GateStatus::Fail,
                    Some(format!("RPC out of sync: {delta} blocks behind tip")),
                )
            };
            gates.push(Gate {
                label: "Chain RPC",
                status,
                value: format!("block {} · Δ +{delta}", cs.block),
                why,
            });
        } else {
            gates.push(unknown("Chain RPC"));
        }

        // 3. Wallet funded -------------------------------------------------
        if let Some(w) = &snap.wallet {
            let zero = BigInt::from(0);
            let bzz = w.bzz_balance.as_ref().unwrap_or(&zero);
            let native = w.native_token_balance.as_ref().unwrap_or(&zero);
            let value = format!("BZZ {bzz} · native {native}");
            if bzz == &zero && native == &zero {
                gates.push(Gate {
                    label: "Wallet funded",
                    status: GateStatus::Fail,
                    value: "0 BZZ · 0 native".into(),
                    why: Some("fund the operator wallet to participate".into()),
                });
            } else if bzz == &zero || native == &zero {
                gates.push(Gate {
                    label: "Wallet funded",
                    status: GateStatus::Warn,
                    value,
                    why: Some("partial funding — need both BZZ (storage) and native (gas)".into()),
                });
            } else {
                gates.push(Gate {
                    label: "Wallet funded",
                    status: GateStatus::Pass,
                    value,
                    why: None,
                });
            }
        } else {
            gates.push(unknown("Wallet funded"));
        }

        // 4. Warmup complete + 5. Peers + 7. Reserve  (all from /status)
        if let Some(s) = &snap.status {
            // 4
            if s.is_warming_up {
                gates.push(Gate {
                    label: "Warmup complete",
                    status: GateStatus::Warn,
                    value: "warming up".into(),
                    why: Some("first-launch warmup can take 5–60 minutes".into()),
                });
            } else {
                gates.push(Gate {
                    label: "Warmup complete",
                    status: GateStatus::Pass,
                    value: "ready".into(),
                    why: None,
                });
            }
            // 5
            let n = s.connected_peers;
            let (pstatus, pwhy) = if n == 0 {
                (GateStatus::Fail, Some("no peers — node is isolated".into()))
            } else if n < 8 {
                (
                    GateStatus::Warn,
                    Some(format!("only {n} connected — bins likely starving")),
                )
            } else {
                (GateStatus::Pass, None)
            };
            gates.push(Gate {
                label: "Peers",
                status: pstatus,
                value: format!("{n} connected"),
                why: pwhy,
            });
            // 7
            let total = s.reserve_size;
            let in_radius = s.reserve_size_within_radius;
            let (rstatus, rwhy) = if total == 0 && !s.is_warming_up {
                (
                    GateStatus::Warn,
                    Some("reserve empty after warmup — check sync rate".into()),
                )
            } else {
                (GateStatus::Pass, None)
            };
            gates.push(Gate {
                label: "Reserve",
                status: rstatus,
                value: format!(
                    "{total} chunks (in-radius: {in_radius}) · radius {}",
                    s.storage_radius
                ),
                why: rwhy,
            });
        } else {
            gates.push(unknown("Warmup complete"));
            gates.push(unknown("Peers"));
            gates.push(unknown("Reserve"));
        }

        // 6. Bin saturation — DEFERRED to v0.2 (needs /topology poller)
        gates.push(Gate {
            label: "Bin saturation",
            status: GateStatus::Unknown,
            value: "(/topology not polled yet)".into(),
            why: Some("v0.2: per-bin starvation detection".into()),
        });

        // 8 / 9 / 10 — redistribution -------------------------------------
        if let Some(r) = &snap.redistribution {
            // 8
            if r.is_healthy {
                gates.push(Gate {
                    label: "Healthy for redistribution",
                    status: GateStatus::Pass,
                    value: "yes".into(),
                    why: None,
                });
            } else if let Some(s) = &snap.status {
                let radius = s.storage_radius;
                let committed = s.committed_depth;
                if radius < committed {
                    gates.push(Gate {
                        label: "Healthy for redistribution",
                        status: GateStatus::Fail,
                        value: format!("storageRadius ({radius}) < committed ({committed})"),
                        why: Some(
                            "storageRadius decreases ONLY on the 30-min reserve worker tick — wait it out or check reserve fill"
                                .into(),
                        ),
                    });
                } else {
                    gates.push(Gate {
                        label: "Healthy for redistribution",
                        status: GateStatus::Fail,
                        value: "isHealthy=false".into(),
                        why: Some("check reserve fill, fully-synced status, freeze status".into()),
                    });
                }
            } else {
                gates.push(Gate {
                    label: "Healthy for redistribution",
                    status: GateStatus::Fail,
                    value: "isHealthy=false".into(),
                    why: None,
                });
            }
            // 9
            if r.is_frozen {
                gates.push(Gate {
                    label: "Not frozen",
                    status: GateStatus::Fail,
                    value: format!("frozen since round {}", r.last_frozen_round),
                    why: Some("invalid commit/reveal or desynced reserve in a recent round".into()),
                });
            } else {
                gates.push(Gate {
                    label: "Not frozen",
                    status: GateStatus::Pass,
                    value: "active".into(),
                    why: None,
                });
            }
            // 10
            if r.has_sufficient_funds {
                gates.push(Gate {
                    label: "Sufficient funds to play",
                    status: GateStatus::Pass,
                    value: "yes".into(),
                    why: None,
                });
            } else {
                gates.push(Gate {
                    label: "Sufficient funds to play",
                    status: GateStatus::Fail,
                    value: "insufficient gas runway".into(),
                    why: Some("top up the operator wallet's native-token balance".into()),
                });
            }
        } else {
            for label in [
                "Healthy for redistribution",
                "Not frozen",
                "Sufficient funds to play",
            ] {
                gates.push(unknown(label));
            }
        }

        gates
    }
}

fn unknown(label: &'static str) -> Gate {
    Gate {
        label,
        status: GateStatus::Unknown,
        value: "—".into(),
        why: None,
    }
}

impl Component for Health {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3), // header
            Constraint::Min(0),    // gates list
            Constraint::Length(1), // footer
        ])
        .split(area);

        // ---- Header --------------------------------------------------
        let header_line1 = Line::from(vec![
            Span::styled("HEALTH", Style::default().add_modifier(Modifier::BOLD)),
            Span::raw("  "),
            Span::styled(
                format!("{} · {}", self.api.name, self.api.url),
                Style::default().fg(Color::Cyan),
            ),
            Span::raw(if self.api.authenticated { "  🔒" } else { "" }),
        ]);
        let mut header_line2 = vec![Span::raw("ping: ")];
        match self.snapshot.last_ping {
            Some(d) => header_line2.push(Span::styled(
                format!("{}ms", d.as_millis()),
                Style::default().fg(Color::Green),
            )),
            None => header_line2.push(Span::styled("—", Style::default().fg(Color::DarkGray))),
        };
        if let Some(err) = &self.snapshot.last_error {
            header_line2.push(Span::raw("  "));
            header_line2.push(Span::styled(
                format!("error: {err}"),
                Style::default().fg(Color::Red),
            ));
        }
        frame.render_widget(
            Paragraph::new(vec![header_line1, Line::from(header_line2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        // ---- Gates ---------------------------------------------------
        let mut lines: Vec<Line> = Vec::new();
        for g in Self::gates_for(&self.snapshot) {
            lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    g.status.glyph(),
                    Style::default()
                        .fg(g.status.color())
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
                            .fg(Color::DarkGray)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), chunks[1]);

        // ---- Footer (keymap) -----------------------------------------
        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
                Span::styled(
                    " Ctrl+C ",
                    Style::default().fg(Color::Black).bg(Color::White),
                ),
                Span::raw(" quit  "),
            ])),
            chunks[2],
        );

        Ok(())
    }
}
