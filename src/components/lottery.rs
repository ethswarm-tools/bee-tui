//! S4 — Lottery / redistribution screen (`docs/PLAN.md` § 8.S4).
//! Pure view-data half lives in [`bee_cockpit_core::views::lottery`];
//! this module owns the API client handle, the watch subscriptions,
//! the rchash benchmark state machine, the scroll cursor, and the
//! ratatui draw path.

use std::sync::Arc;

use color_eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    Frame,
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph},
};
use tokio::sync::{mpsc, watch};

pub use bee_cockpit_core::views::lottery::{
    AnchorRow, BENCH_DEFAULT_DEPTH, BLOCKS_PER_PHASE, BLOCKS_PER_ROUND, LotteryView, Phase,
    PhaseSegment, PhaseState, RoundCard, StakeCard, StakeStatus, bench_depth, build_phase_segments,
    format_when, view_for,
};

use super::Component;
use crate::action::Action;
use crate::api::ApiClient;
use crate::theme;
use crate::watch::{HealthSnapshot, LotterySnapshot};

use bee::debug::RCHashResponse;

fn stake_color(s: StakeStatus) -> Color {
    match s {
        StakeStatus::Unstaked => theme::active().fail,
        StakeStatus::InsufficientGas => theme::active().warn,
        StakeStatus::Frozen => theme::active().fail,
        StakeStatus::Unhealthy => theme::active().warn,
        StakeStatus::Healthy => theme::active().pass,
        StakeStatus::Unknown => theme::active().dim,
    }
}

/// Lifecycle of the on-demand rchash benchmark.
#[derive(Debug, Clone, PartialEq)]
pub enum BenchState {
    Idle,
    Running,
    Done { duration_seconds: f64, hash: String },
    Failed { error: String },
}

const BENCH_ANCHOR_LO: &str = "0000000000000000000000000000000000000000000000000000000000000000";
const BENCH_ANCHOR_HI: &str = "ffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffffff";

pub struct Lottery {
    client: Arc<ApiClient>,
    health_rx: watch::Receiver<HealthSnapshot>,
    lottery_rx: watch::Receiver<LotterySnapshot>,
    health: HealthSnapshot,
    lottery: LotterySnapshot,
    bench: BenchState,
    bench_tx: mpsc::UnboundedSender<Result<RCHashResponse, String>>,
    bench_rx: mpsc::UnboundedReceiver<Result<RCHashResponse, String>>,
    scroll_offset: usize,
}

impl Lottery {
    pub fn new(
        client: Arc<ApiClient>,
        health_rx: watch::Receiver<HealthSnapshot>,
        lottery_rx: watch::Receiver<LotterySnapshot>,
    ) -> Self {
        let health = health_rx.borrow().clone();
        let lottery = lottery_rx.borrow().clone();
        let (bench_tx, bench_rx) = mpsc::unbounded_channel();
        Self {
            client,
            health_rx,
            lottery_rx,
            health,
            lottery,
            bench: BenchState::Idle,
            bench_tx,
            bench_rx,
            scroll_offset: 0,
        }
    }

    /// Re-export of core's pure view computation as an inherent
    /// function so existing `Lottery::view_for` call sites resolve.
    pub fn view_for(health: &HealthSnapshot, lottery: &LotterySnapshot) -> LotteryView {
        view_for(health, lottery)
    }

    fn pull_latest(&mut self) {
        self.health = self.health_rx.borrow().clone();
        self.lottery = self.lottery_rx.borrow().clone();
    }

    fn drain_bench_results(&mut self) {
        while let Ok(result) = self.bench_rx.try_recv() {
            self.bench = match result {
                Ok(resp) => BenchState::Done {
                    duration_seconds: resp.duration_seconds,
                    hash: resp.hash,
                },
                Err(e) => BenchState::Failed { error: e },
            };
        }
    }

    fn maybe_start_bench(&mut self) -> bool {
        if matches!(self.bench, BenchState::Running) {
            return false;
        }
        let depth = bench_depth(&self.health);
        let client = self.client.clone();
        let tx = self.bench_tx.clone();
        tokio::spawn(async move {
            let res = client
                .bee()
                .debug()
                .r_chash(depth, BENCH_ANCHOR_LO, BENCH_ANCHOR_HI)
                .await
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
        self.bench = BenchState::Running;
        true
    }
}

impl Component for Lottery {
    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        if matches!(action, Action::Tick) {
            self.pull_latest();
            self.drain_bench_results();
        }
        Ok(None)
    }

    fn handle_key_event(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if matches!(key.code, KeyCode::Char('r')) {
            self.maybe_start_bench();
        } else {
            self.scroll_offset = super::scroll::scroll_key(self.scroll_offset, key.code);
        }
        Ok(None)
    }

    fn draw(&mut self, frame: &mut Frame, area: Rect) -> Result<()> {
        let chunks = Layout::vertical([
            Constraint::Length(3),
            Constraint::Length(6),
            Constraint::Length(7),
            Constraint::Min(0),
            Constraint::Length(1),
        ])
        .split(area);

        let header_l1 = Line::from(vec![Span::styled(
            "LOTTERY / REDISTRIBUTION",
            Style::default().add_modifier(Modifier::BOLD),
        )]);
        let mut header_l2 = Vec::new();
        let t = theme::active();
        if let Some(err) = &self.lottery.last_error {
            let (color, msg) = theme::classify_header_error(err);
            header_l2.push(Span::styled(msg, Style::default().fg(color)));
        } else if !self.lottery.is_loaded() {
            header_l2.push(Span::styled(
                format!("{} loading…", theme::spinner_glyph()),
                Style::default().fg(t.dim),
            ));
        }
        frame.render_widget(
            Paragraph::new(vec![header_l1, Line::from(header_l2)])
                .block(Block::default().borders(Borders::BOTTOM)),
            chunks[0],
        );

        let view = view_for(&self.health, &self.lottery);

        let mut round_lines: Vec<Line> = Vec::new();
        if let Some(rc) = &view.round {
            round_lines.push(Line::from(vec![
                Span::styled(
                    format!("  Round {} ", rc.round),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!(
                        "· phase {} · block-of-round {}/{BLOCKS_PER_ROUND}",
                        rc.phase_label, rc.block_of_round
                    ),
                    Style::default().fg(t.dim),
                ),
            ]));
            round_lines.push(Line::from(segment_spans(&rc.segments)));
            round_lines.push(Line::from(progress_bar_spans(rc)));
        } else {
            round_lines.push(Line::from(Span::styled(
                "  (redistribution state not loaded yet)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        }
        frame.render_widget(
            Paragraph::new(round_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[1],
        );

        let mut anchor_lines: Vec<Line> = vec![Line::from(Span::styled(
            "  ANCHORS         ROUND       WHEN",
            Style::default().fg(t.dim).add_modifier(Modifier::BOLD),
        ))];
        if view.anchors.is_empty() {
            anchor_lines.push(Line::from(Span::styled(
                "  (no anchor data)",
                Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
            )));
        } else {
            for a in &view.anchors {
                let round_str = if a.round == 0 {
                    "—".to_string()
                } else {
                    a.round.to_string()
                };
                anchor_lines.push(Line::from(vec![
                    Span::raw("  "),
                    Span::styled(
                        format!("{:<14} ", a.label),
                        Style::default().add_modifier(Modifier::BOLD),
                    ),
                    Span::raw(format!("{round_str:<11} ")),
                    Span::styled(a.when.clone(), Style::default().fg(t.dim)),
                ]));
            }
        }
        frame.render_widget(
            Paragraph::new(anchor_lines).block(Block::default().borders(Borders::BOTTOM)),
            chunks[2],
        );

        let stake = &view.stake;
        let mut stake_lines = vec![
            Line::from(vec![
                Span::styled("  Stake  ", Style::default().add_modifier(Modifier::BOLD)),
                Span::styled(
                    stake.status.label(),
                    Style::default()
                        .fg(stake_color(stake.status))
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(vec![
                Span::raw(format!("    staked     {}", stake.staked)),
                Span::raw("   "),
                Span::raw(format!("min gas funds  {}", stake.minimum_gas)),
            ]),
            Line::from(vec![
                Span::raw(format!("    reward     {}", stake.reward)),
                Span::raw("   "),
                Span::raw(format!("fees           {}", stake.fees)),
            ]),
        ];

        // Derived economics: is playing the lottery actually paying off?
        let econ = &view.economics;
        let net_style = if econ.net_negative {
            Style::default().fg(theme::active().fail)
        } else {
            Style::default().fg(t.info)
        };
        let roi = econ.roi_pct.clone().unwrap_or_else(|| "—".to_string());
        stake_lines.push(Line::from(vec![
            Span::raw("    net reward "),
            Span::styled(econ.net_reward.clone(), net_style),
            Span::raw("   "),
            Span::raw(format!("ROI            {roi}")),
        ]));
        if let Some(n) = econ.rounds_since_win {
            stake_lines.push(Line::from(Span::styled(
                format!("    {n} rounds since last win"),
                Style::default().fg(t.dim),
            )));
        }

        if let Some(sample) = &stake.last_sample {
            stake_lines.push(Line::from(vec![
                Span::raw("    last sample "),
                Span::styled(sample.clone(), Style::default().fg(t.info)),
                Span::styled(
                    "   (deadline ≈ 95s for the 38-block commit window)",
                    Style::default().fg(t.dim),
                ),
            ]));
        }
        if let Some(why) = &stake.why {
            stake_lines.push(Line::from(vec![
                Span::raw("    └─ "),
                Span::styled(
                    why.clone(),
                    Style::default().fg(t.dim).add_modifier(Modifier::ITALIC),
                ),
            ]));
        }
        let depth = bench_depth(&self.health);
        stake_lines.push(Line::from(""));
        stake_lines.push(Line::from(vec![
            Span::styled(
                "  rchash bench  ",
                Style::default().add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("(depth {depth}, deterministic anchors)"),
                Style::default().fg(t.dim),
            ),
        ]));
        match &self.bench {
            BenchState::Idle => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled("press 'r' to run a sample", Style::default().fg(t.dim)),
                ]));
            }
            BenchState::Running => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(
                        "running… (this can take seconds-to-minutes on a busy reserve)",
                        Style::default().fg(t.info),
                    ),
                ]));
            }
            BenchState::Done {
                duration_seconds,
                hash,
            } => {
                let safe = *duration_seconds < 95.0;
                let style = if safe {
                    Style::default().fg(t.pass)
                } else {
                    Style::default().fg(t.fail).add_modifier(Modifier::BOLD)
                };
                let verdict = if safe {
                    "safe — fits inside the 95 s commit window"
                } else {
                    "OVER 95 s commit window — sampler will time out!"
                };
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("{duration_seconds:.1}s"), style),
                    Span::raw("   "),
                    Span::styled(verdict, Style::default().fg(t.dim)),
                ]));
                let trimmed = hash.trim_start_matches("0x");
                stake_lines.push(Line::from(vec![
                    Span::styled("       hash 0x", Style::default().fg(t.dim)),
                    Span::styled(trimmed.to_string(), Style::default().fg(t.info)),
                ]));
            }
            BenchState::Failed { error } => {
                stake_lines.push(Line::from(vec![
                    Span::raw("    "),
                    Span::styled(format!("error: {error}"), Style::default().fg(t.fail)),
                ]));
            }
        }
        let stake_area = chunks[3];
        let visible_rows = stake_area.height as usize;
        let stake_total = stake_lines.len();
        self.scroll_offset =
            super::scroll::clamp_offset(self.scroll_offset, visible_rows, stake_total);
        frame.render_widget(
            Paragraph::new(stake_lines).scroll((self.scroll_offset as u16, 0)),
            stake_area,
        );
        super::scroll::render_scrollbar(
            frame,
            stake_area,
            self.scroll_offset,
            visible_rows,
            stake_total,
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓ ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" scroll  "),
                Span::styled(" Tab ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" switch screen  "),
                Span::styled(" r ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" run rchash benchmark  "),
                Span::styled(" ? ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" help  "),
                Span::styled(" q ", Style::default().fg(Color::Black).bg(Color::White)),
                Span::raw(" quit  "),
            ])),
            chunks[4],
        );

        Ok(())
    }
}

fn segment_spans(segs: &[PhaseSegment]) -> Vec<Span<'static>> {
    let t = theme::active();
    let mut out = vec![Span::raw("  ")];
    for (i, s) in segs.iter().enumerate() {
        let color = match s.state {
            PhaseState::Done => t.dim,
            PhaseState::Active => t.warn,
            PhaseState::Pending => Color::White,
        };
        let modifier = if matches!(s.state, PhaseState::Active) {
            Modifier::BOLD
        } else {
            Modifier::empty()
        };
        out.push(Span::styled(
            format!(" {} {}..{} ", s.phase.label(), s.start_block, s.end_block),
            Style::default().fg(color).add_modifier(modifier),
        ));
        if i + 1 < segs.len() {
            out.push(Span::styled("│", Style::default().fg(t.dim)));
        }
    }
    out
}

fn progress_bar_spans(rc: &RoundCard) -> Vec<Span<'static>> {
    const WIDTH: usize = 24;
    let filled = ((rc.block_of_round as usize) * WIDTH) / BLOCKS_PER_ROUND as usize;
    let mut bar = String::with_capacity(WIDTH);
    for _ in 0..filled.min(WIDTH) {
        bar.push('▇');
    }
    for _ in filled.min(WIDTH)..WIDTH {
        bar.push('░');
    }
    vec![
        Span::raw("  "),
        Span::styled(bar, Style::default().fg(theme::active().warn)),
        Span::raw(format!("   {}/{BLOCKS_PER_ROUND}", rc.block_of_round)),
    ]
}
