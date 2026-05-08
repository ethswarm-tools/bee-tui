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
use crate::theme;
use crate::watch::{HealthSnapshot, TopologySnapshot};

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
        let g = theme::active().glyphs;
        match self {
            Self::Pass => g.pass,
            Self::Warn => g.warn,
            Self::Fail => g.fail,
            Self::Unknown => g.bullet,
        }
    }
    fn color(self) -> Color {
        let t = theme::active();
        match self {
            Self::Pass => t.pass,
            Self::Warn => t.warn,
            Self::Fail => t.fail,
            Self::Unknown => t.dim,
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
/// from the [`crate::watch::BeeWatch`] hub plus the [`TopologySnapshot`]
/// stream that drives the bin-saturation gate.
pub struct Health {
    api: Arc<ApiClient>,
    rx: watch::Receiver<HealthSnapshot>,
    topology_rx: watch::Receiver<TopologySnapshot>,
    snapshot: HealthSnapshot,
    topology: TopologySnapshot,
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
        }
    }

    fn pull_latest(&mut self) {
        self.snapshot = self.rx.borrow().clone();
        self.topology = self.topology_rx.borrow().clone();
    }

    /// Pure, snapshot-driven gate computation. Exposed for snapshot
    /// tests so they can stub the inputs and assert the resulting
    /// gate list without a running app loop.
    pub fn gates_for(snap: &HealthSnapshot, topology: Option<&TopologySnapshot>) -> Vec<Gate> {
        Self::gates_for_with_stamps(snap, topology, None)
    }

    /// Same as [`Self::gates_for`] but with an optional stamps
    /// snapshot — when present, the returned list includes a
    /// "Stamp TTL" gate aggregating the worst usable batch's
    /// remaining TTL. Plumbed separately so the existing visual
    /// `Health` screen (which doesn't pull stamps) keeps the same
    /// gate count it had before; the alerter and `:diagnose` bundle
    /// pass the snapshot in.
    pub fn gates_for_with_stamps(
        snap: &HealthSnapshot,
        topology: Option<&TopologySnapshot>,
        stamps: Option<&crate::watch::StampsSnapshot>,
    ) -> Vec<Gate> {
        let mut gates = Self::gates_for_inner(snap, topology);
        if let Some(s) = stamps {
            gates.push(stamp_ttl_gate(s));
        }
        gates
    }

    fn gates_for_inner(
        snap: &HealthSnapshot,
        topology: Option<&TopologySnapshot>,
    ) -> Vec<Gate> {
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

        // 6. Bin saturation — derived from /topology populations vs
        // the bee-go SaturationPeers=8 constant. We flag any bin at
        // or below the kademlia depth that has fewer than 8
        // connected peers; far bins past the depth are expected to
        // be sparse and don't trigger this gate.
        gates.push(bin_saturation_gate(topology));

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

/// "Stamp TTL" gate. Aggregates over usable batches (`usable=true`,
/// non-empty TTL) and reports the worst-case bucket. Pending batches
/// (`usable=false`) and zero-batch nodes are reported as Unknown
/// (no opinion) rather than Pass — operators on a fresh node would
/// be surprised by a green stamp gate when no batches exist.
fn stamp_ttl_gate(s: &crate::watch::StampsSnapshot) -> Gate {
    if s.last_update.is_none() {
        return unknown("Stamp TTL");
    }
    let usable: Vec<&bee::postage::PostageBatch> =
        s.batches.iter().filter(|b| b.usable).collect();
    if usable.is_empty() {
        return Gate {
            label: "Stamp TTL",
            status: GateStatus::Unknown,
            value: "no usable batches".into(),
            why: None,
        };
    }
    let worst = usable.iter().min_by_key(|b| b.batch_ttl).copied().unwrap();
    let ttl = worst.batch_ttl;
    let hex = worst.batch_id.to_hex();
    let id_short: &str = if hex.len() > 8 { &hex[..8] } else { &hex };
    let value = format!(
        "worst-batch {id_short} · TTL {}",
        crate::components::stamps::format_ttl_seconds(ttl),
    );
    if ttl <= crate::components::stamps::TOPUP_URGENT_SECS {
        Gate {
            label: "Stamp TTL",
            status: GateStatus::Fail,
            value,
            why: Some(format!(
                "topup URGENT — under {}h threshold",
                crate::components::stamps::TOPUP_URGENT_SECS / 3600
            )),
        }
    } else if ttl <= crate::components::stamps::TOPUP_SOON_SECS {
        Gate {
            label: "Stamp TTL",
            status: GateStatus::Warn,
            value,
            why: Some(format!(
                "topup soon — under {}d planning threshold",
                crate::components::stamps::TOPUP_SOON_SECS / 86_400
            )),
        }
    } else {
        Gate {
            label: "Stamp TTL",
            status: GateStatus::Pass,
            value,
            why: None,
        }
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

/// Threshold for the bin-saturation gate. Mirrors bee-go's
/// `SaturationPeers` constant (`pkg/topology/kademlia/kademlia.go:54`).
const SATURATION_PEERS: u64 = 8;
/// Cap on the number of starving bin numbers listed inline in the
/// gate's value string. Avoids one mega-line when a brand-new node
/// reports every bin as starving.
const STARVING_LIST_CAP: usize = 5;

fn bin_saturation_gate(topology: Option<&TopologySnapshot>) -> Gate {
    let Some(snap) = topology else {
        return unknown("Bin saturation");
    };
    if let Some(err) = &snap.last_error {
        return Gate {
            label: "Bin saturation",
            status: GateStatus::Unknown,
            value: format!("topology error: {err}"),
            why: None,
        };
    }
    let Some(t) = &snap.topology else {
        return unknown("Bin saturation");
    };
    // Only flag bins at or below the kademlia depth — bins beyond
    // depth are expected to be sparse during normal operation.
    let starving: Vec<u8> = t
        .bins
        .iter()
        .enumerate()
        .filter_map(|(i, b)| {
            let bin = i as u8;
            if bin <= t.depth && b.connected < SATURATION_PEERS {
                Some(bin)
            } else {
                None
            }
        })
        .collect();
    if starving.is_empty() {
        Gate {
            label: "Bin saturation",
            status: GateStatus::Pass,
            value: format!(
                "all bins ≤ depth ({}) saturated (≥{SATURATION_PEERS})",
                t.depth
            ),
            why: None,
        }
    } else {
        let listed: Vec<String> = starving
            .iter()
            .take(STARVING_LIST_CAP)
            .map(|b| format!("bin {b}"))
            .collect();
        let suffix = if starving.len() > STARVING_LIST_CAP {
            format!(" (+{} more)", starving.len() - STARVING_LIST_CAP)
        } else {
            String::new()
        };
        Gate {
            label: "Bin saturation",
            status: GateStatus::Warn,
            value: format!(
                "{} starving: {}{suffix}",
                starving.len(),
                listed.join(", ")
            ),
            why: Some(
                "manually `connect` more peers or wait — kademlia fills bins as the node sees more traffic"
                    .into(),
            ),
        }
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

        // ---- Gates ---------------------------------------------------
        let mut lines: Vec<Line> = Vec::new();
        for g in Self::gates_for(&self.snapshot, Some(&self.topology)) {
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
                            .fg(theme::active().dim)
                            .add_modifier(Modifier::ITALIC),
                    ),
                ]));
            }
        }
        frame.render_widget(Paragraph::new(lines), chunks[1]);

        // ---- Footer (keymap) -----------------------------------------
        frame.render_widget(
            Paragraph::new(Line::from(vec![
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

#[cfg(test)]
mod stamp_ttl_tests {
    use super::*;
    use crate::components::stamps::{TOPUP_SOON_SECS, TOPUP_URGENT_SECS};
    use crate::watch::StampsSnapshot;
    use bee::postage::PostageBatch;
    use std::time::Instant;

    fn batch(ttl_secs: i64, usable: bool) -> PostageBatch {
        PostageBatch {
            batch_id: bee::swarm::BatchId::new(&[0xab; 32]).unwrap(),
            amount: None,
            start: 0,
            owner: String::new(),
            depth: 22,
            bucket_depth: 16,
            immutable: true,
            batch_ttl: ttl_secs,
            utilization: 0,
            usable,
            exists: true,
            label: "test".into(),
            block_number: 0,
        }
    }

    fn loaded(batches: Vec<PostageBatch>) -> StampsSnapshot {
        StampsSnapshot {
            batches,
            last_error: None,
            last_update: Some(Instant::now()),
        }
    }

    #[test]
    fn stamp_ttl_unknown_when_not_loaded() {
        let snap = StampsSnapshot::default();
        let g = stamp_ttl_gate(&snap);
        assert_eq!(g.status, GateStatus::Unknown);
    }

    #[test]
    fn stamp_ttl_unknown_when_no_usable_batches() {
        // Pending batches don't count.
        let snap = loaded(vec![batch(30 * 86_400, false)]);
        let g = stamp_ttl_gate(&snap);
        assert_eq!(g.status, GateStatus::Unknown);
        assert!(g.value.contains("no usable"));
    }

    #[test]
    fn stamp_ttl_pass_when_all_above_planning_threshold() {
        let snap = loaded(vec![batch(30 * 86_400, true), batch(10 * 86_400, true)]);
        let g = stamp_ttl_gate(&snap);
        assert_eq!(g.status, GateStatus::Pass);
    }

    #[test]
    fn stamp_ttl_warn_when_within_planning_window() {
        // 3 days < 7d planning threshold but > 24h urgent threshold.
        let ttl = 3 * 86_400;
        assert!(ttl <= TOPUP_SOON_SECS);
        assert!(ttl > TOPUP_URGENT_SECS);
        let snap = loaded(vec![batch(30 * 86_400, true), batch(ttl, true)]);
        let g = stamp_ttl_gate(&snap);
        assert_eq!(g.status, GateStatus::Warn);
        // Worst-batch wins.
        assert!(g.value.contains("3d") || g.value.contains("72h"));
    }

    #[test]
    fn stamp_ttl_fail_when_under_urgent_threshold() {
        let snap = loaded(vec![batch(30 * 86_400, true), batch(12 * 3600, true)]);
        let g = stamp_ttl_gate(&snap);
        assert_eq!(g.status, GateStatus::Fail);
    }

    #[test]
    fn gates_for_with_stamps_appends_one_extra_gate() {
        let snap = HealthSnapshot::default();
        let baseline = Health::gates_for(&snap, None);
        let with_stamps =
            Health::gates_for_with_stamps(&snap, None, Some(&StampsSnapshot::default()));
        assert_eq!(with_stamps.len(), baseline.len() + 1);
        assert_eq!(with_stamps.last().unwrap().label, "Stamp TTL");
    }
}
