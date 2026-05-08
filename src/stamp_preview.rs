//! Pure math + formatting for the four `:*-preview` command-bar
//! verbs (`:topup-preview`, `:dilute-preview`, `:extend-preview`,
//! `:buy-preview`).
//!
//! All functions here are **read-only** — they compute what *would*
//! happen if the operator ran a write op against Bee, without
//! actually issuing one. This honours bee-tui's "Truth over API,
//! read-mostly" stance while still giving operators the predictive
//! answers they normally have to leave the cockpit for.
//!
//! ## Formulas
//!
//! Every formula here is the canonical one used across swarm-cli
//! (`stamp/buy.ts:97-103`), beekeeper-stamper
//! (`pkg/stamper/node.go:33-43,69-115`), gateway-proxy
//! (`stamps.ts:198-234`), and bee-scripts (`calculate_bzz.sh`):
//!
//! - `cost_bzz   = amount × 2^depth / 1e16`
//! - `ttl_blocks = amount / current_price`
//! - `ttl_secs   = ttl_blocks × blocktime_seconds`
//! - `capacity   = 2^depth × 4096` bytes
//! - `dilute(d → d+k)`: `new_amount = old_amount / 2^k`,
//!   `new_ttl = old_ttl / 2^k`, `new_capacity = capacity × 2^k`,
//!   cost = 0 (no new BZZ paid; the existing balance is
//!   redistributed across more chunks)
//!
//! ## Why a separate module
//!
//! The drill-pane economics in [`crate::components::stamps`] use the
//! same formulas but only for an *existing* batch. Previews extend
//! that to hypothetical batches (`:buy-preview`) and hypothetical
//! changes (`:topup-preview`, `:dilute-preview`,
//! `:extend-preview`). Splitting them out keeps the per-screen file
//! focused on its render path.

use bee::debug::ChainState;
use bee::postage::PostageBatch;
use num_bigint::BigInt;

use crate::components::stamps::{format_bytes, format_ttl_seconds};

/// Block time in seconds for Gnosis Chain (where Bee's stamp
/// contract lives). Hard-coded across the ecosystem (swarm-cli,
/// beekeeper, gateway-proxy all assume 5s); pinning it here matches.
/// Exposed as a constant so tests can reuse it.
pub const GNOSIS_BLOCK_TIME_SECS: i64 = 5;

/// 1 BZZ in PLUR.
pub const PLUR_PER_BZZ: f64 = 1e16;

/// Outcome of `:topup-preview`. Pure values; the verb formats them
/// into the single-line `CommandStatus` summary.
#[derive(Debug, Clone, PartialEq)]
pub struct TopupPreview {
    pub batch_id_short: String,
    pub current_depth: u8,
    pub current_ttl_seconds: i64,
    /// Additional per-chunk PLUR the operator wants to add.
    pub delta_amount: BigInt,
    /// Extra TTL the topup would buy, in seconds.
    pub extra_ttl_seconds: i64,
    /// New TTL = current + extra (clamped at 0 if Bee already
    /// reports the batch as expired).
    pub new_ttl_seconds: i64,
    /// Cost of the topup in BZZ (= delta × 2^depth / 1e16).
    pub cost_bzz: f64,
}

impl TopupPreview {
    /// One-line summary for the command-bar.
    pub fn summary(&self) -> String {
        format!(
            "topup-preview {}: +{:.4} BZZ (delta {} PLUR/chunk), TTL {} → {}",
            self.batch_id_short,
            self.cost_bzz,
            self.delta_amount,
            format_ttl_seconds(self.current_ttl_seconds),
            format_ttl_seconds(self.new_ttl_seconds),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct DilutePreview {
    pub batch_id_short: String,
    pub old_depth: u8,
    pub new_depth: u8,
    pub old_capacity_bytes: u128,
    pub new_capacity_bytes: u128,
    pub old_ttl_seconds: i64,
    pub new_ttl_seconds: i64,
}

impl DilutePreview {
    pub fn summary(&self) -> String {
        format!(
            "dilute-preview {}: depth {}→{}, capacity {}→{}, TTL {}→{}, cost 0 BZZ",
            self.batch_id_short,
            self.old_depth,
            self.new_depth,
            format_bytes(self.old_capacity_bytes),
            format_bytes(self.new_capacity_bytes),
            format_ttl_seconds(self.old_ttl_seconds),
            format_ttl_seconds(self.new_ttl_seconds),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ExtendPreview {
    pub batch_id_short: String,
    pub depth: u8,
    pub current_ttl_seconds: i64,
    pub extension_seconds: i64,
    /// Per-chunk PLUR the operator would need to add to gain
    /// `extension_seconds` of TTL at the current price.
    pub needed_amount_plur: BigInt,
    pub cost_bzz: f64,
    pub new_ttl_seconds: i64,
}

impl ExtendPreview {
    pub fn summary(&self) -> String {
        format!(
            "extend-preview {} +{}: cost {:.4} BZZ ({} PLUR/chunk), TTL {} → {}",
            self.batch_id_short,
            format_ttl_seconds(self.extension_seconds),
            self.cost_bzz,
            self.needed_amount_plur,
            format_ttl_seconds(self.current_ttl_seconds),
            format_ttl_seconds(self.new_ttl_seconds),
        )
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BuyPreview {
    pub depth: u8,
    pub amount_plur: BigInt,
    pub capacity_bytes: u128,
    pub ttl_seconds: i64,
    pub cost_bzz: f64,
}

impl BuyPreview {
    pub fn summary(&self) -> String {
        format!(
            "buy-preview depth={} amount={} PLUR/chunk: capacity {}, TTL {}, cost {:.4} BZZ",
            self.depth,
            self.amount_plur,
            format_bytes(self.capacity_bytes),
            format_ttl_seconds(self.ttl_seconds),
            self.cost_bzz,
        )
    }
}

/// Output of `:plan-batch` — the unified topup+dilute decision the
/// beekeeper-stamper module makes per-batch. Given thresholds and a
/// batch, returns whether topup and/or dilute is needed and the BZZ
/// cost of each leg. Each leg is independent; the cockpit shows them
/// together so the operator sees the full picture at once.
///
/// Mirrors `pkg/stamper/node.go:Set` in beekeeper, except read-only
/// (no chain writes). Default thresholds match the cross-ecosystem
/// convention (swarm-gateway): usage 0.85, TTL 24h, dilute by +2.
#[derive(Debug, Clone, PartialEq)]
pub struct PlanPreview {
    pub batch_id_short: String,
    /// Snapshot of the batch's state at plan time.
    pub current_depth: u8,
    pub current_usage_pct: f64,
    pub current_ttl_seconds: i64,
    /// Threshold inputs the plan was computed against.
    pub usage_threshold_pct: f64,
    pub ttl_threshold_seconds: i64,
    pub extra_depth: u8,
    /// Action recommended: `None` when nothing to do, otherwise zero
    /// or more legs that together restore the batch to thresholds.
    pub action: PlanAction,
    /// Total BZZ cost across both legs. Dilute is free; topup pays.
    pub total_cost_bzz: f64,
    /// Reason rendered alongside the plan ("usage above 85%", "TTL
    /// below 24h after dilute", "immutable batch can't dilute", etc.)
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PlanAction {
    /// Batch is healthy against both thresholds — no action needed.
    None,
    /// Just topup; usage is still under threshold.
    Topup {
        delta_amount_plur: BigInt,
        new_ttl_seconds: i64,
        cost_bzz: f64,
    },
    /// Just dilute; TTL is still above threshold post-dilute.
    Dilute {
        new_depth: u8,
        post_dilute_ttl_seconds: i64,
    },
    /// Both. Topup happens first (preserves the per-chunk amount),
    /// then dilute drops the post-topup TTL by `2^extra_depth`.
    TopupThenDilute {
        topup_delta_amount_plur: BigInt,
        topup_cost_bzz: f64,
        new_depth: u8,
        post_dilute_ttl_seconds: i64,
    },
}

impl PlanPreview {
    pub fn summary(&self) -> String {
        let action_line = match &self.action {
            PlanAction::None => "no action needed".to_string(),
            PlanAction::Topup {
                delta_amount_plur,
                cost_bzz,
                new_ttl_seconds,
                ..
            } => format!(
                "topup +{} PLUR/chunk → TTL {} (cost {cost_bzz:.4} BZZ)",
                delta_amount_plur,
                format_ttl_seconds(*new_ttl_seconds),
            ),
            PlanAction::Dilute {
                new_depth,
                post_dilute_ttl_seconds,
            } => format!(
                "dilute → depth {new_depth} (TTL {} after, no BZZ)",
                format_ttl_seconds(*post_dilute_ttl_seconds),
            ),
            PlanAction::TopupThenDilute {
                topup_delta_amount_plur,
                topup_cost_bzz,
                new_depth,
                post_dilute_ttl_seconds,
            } => format!(
                "topup +{topup_delta_amount_plur} PLUR/chunk + dilute → depth {new_depth} (TTL {} after, cost {topup_cost_bzz:.4} BZZ)",
                format_ttl_seconds(*post_dilute_ttl_seconds),
            ),
        };
        format!(
            "plan-batch {}: usage {:.1}% (thr {:.0}%), TTL {} (thr {}); {action_line}; total {:.4} BZZ — {}",
            self.batch_id_short,
            self.current_usage_pct * 100.0,
            self.usage_threshold_pct * 100.0,
            format_ttl_seconds(self.current_ttl_seconds),
            format_ttl_seconds(self.ttl_threshold_seconds),
            self.total_cost_bzz,
            self.reason,
        )
    }
}

/// Default thresholds, sourced from gateway-proxy and swarm-gateway:
/// trigger dilute when usage exceeds 85%, top-up when remaining TTL
/// drops below 24 hours, dilute by +2 depth (4× capacity).
pub const DEFAULT_USAGE_THRESHOLD: f64 = 0.85;
pub const DEFAULT_TTL_THRESHOLD_SECONDS: i64 = 24 * 60 * 60;
pub const DEFAULT_EXTRA_DEPTH: u8 = 2;

/// Run beekeeper-stamper's `Set` algorithm read-only on a single
/// batch. Decides whether to topup, dilute, both, or skip.
///
/// * `usage_threshold` — fraction in `[0, 1]`. Above this, dilute.
/// * `ttl_threshold_seconds` — if remaining TTL (after any dilute)
///   is below this, topup to bring it back above.
/// * `extra_depth` — how many depth levels to dilute by when needed.
pub fn plan_batch(
    batch: &PostageBatch,
    chain_state: &ChainState,
    usage_threshold: f64,
    ttl_threshold_seconds: i64,
    extra_depth: u8,
) -> Result<PlanPreview, String> {
    if !(0.0..=1.0).contains(&usage_threshold) {
        return Err(format!(
            "usage_threshold {usage_threshold} out of range [0, 1]"
        ));
    }
    if ttl_threshold_seconds <= 0 {
        return Err("ttl_threshold must be a positive duration".into());
    }
    if chain_state.current_price <= BigInt::from(0) {
        return Err("chain price not loaded yet — try again in a moment".into());
    }
    let bucket_depth = batch.bucket_depth.max(16);
    let usage_pct = stamp_usage(batch.utilization, batch.depth, bucket_depth);
    let current_ttl = batch.batch_ttl.max(0);

    let new_depth = batch.depth.saturating_add(extra_depth);
    if new_depth > 41 {
        return Err(format!(
            "current depth {} + extra_depth {extra_depth} exceeds Bee's depth ceiling 41",
            batch.depth
        ));
    }

    // Dilute leg: when usage exceeds threshold + the batch is
    // mutable. Each +1 depth halves the remaining TTL.
    let needs_dilute = usage_pct >= usage_threshold;
    let dilute_factor = 1i64 << extra_depth;
    let post_dilute_ttl = current_ttl / dilute_factor.max(1);

    if batch.immutable && needs_dilute {
        // Immutable batches can't dilute — flag and only consider topup.
        if current_ttl >= ttl_threshold_seconds {
            return Ok(PlanPreview {
                batch_id_short: short_batch_id(batch),
                current_depth: batch.depth,
                current_usage_pct: usage_pct,
                current_ttl_seconds: current_ttl,
                usage_threshold_pct: usage_threshold,
                ttl_threshold_seconds,
                extra_depth,
                action: PlanAction::None,
                total_cost_bzz: 0.0,
                reason: format!(
                    "immutable batch above usage threshold ({:.1}%) — can't dilute, but TTL still above threshold",
                    usage_pct * 100.0
                ),
            });
        }
        let needed = ttl_threshold_seconds.saturating_sub(current_ttl).max(1);
        let amount =
            amount_for_ttl_extension(needed, &chain_state.current_price, GNOSIS_BLOCK_TIME_SECS);
        let cost = cost_bzz(&amount, batch.depth);
        return Ok(PlanPreview {
            batch_id_short: short_batch_id(batch),
            current_depth: batch.depth,
            current_usage_pct: usage_pct,
            current_ttl_seconds: current_ttl,
            usage_threshold_pct: usage_threshold,
            ttl_threshold_seconds,
            extra_depth,
            action: PlanAction::Topup {
                delta_amount_plur: amount,
                new_ttl_seconds: current_ttl + needed,
                cost_bzz: cost,
            },
            total_cost_bzz: cost,
            reason: "immutable batch above usage threshold + TTL below threshold — topup only"
                .to_string(),
        });
    }

    let effective_ttl_after = if needs_dilute {
        post_dilute_ttl
    } else {
        current_ttl
    };
    let needs_topup = effective_ttl_after < ttl_threshold_seconds;

    match (needs_topup, needs_dilute) {
        (false, false) => Ok(PlanPreview {
            batch_id_short: short_batch_id(batch),
            current_depth: batch.depth,
            current_usage_pct: usage_pct,
            current_ttl_seconds: current_ttl,
            usage_threshold_pct: usage_threshold,
            ttl_threshold_seconds,
            extra_depth,
            action: PlanAction::None,
            total_cost_bzz: 0.0,
            reason: "batch is healthy against both thresholds".into(),
        }),
        (true, false) => {
            let needed = ttl_threshold_seconds.saturating_sub(current_ttl).max(1);
            let amount = amount_for_ttl_extension(
                needed,
                &chain_state.current_price,
                GNOSIS_BLOCK_TIME_SECS,
            );
            let cost = cost_bzz(&amount, batch.depth);
            Ok(PlanPreview {
                batch_id_short: short_batch_id(batch),
                current_depth: batch.depth,
                current_usage_pct: usage_pct,
                current_ttl_seconds: current_ttl,
                usage_threshold_pct: usage_threshold,
                ttl_threshold_seconds,
                extra_depth,
                action: PlanAction::Topup {
                    delta_amount_plur: amount,
                    new_ttl_seconds: current_ttl + needed,
                    cost_bzz: cost,
                },
                total_cost_bzz: cost,
                reason: format!(
                    "TTL below threshold ({}) — topup",
                    format_ttl_seconds(ttl_threshold_seconds)
                ),
            })
        }
        (false, true) => Ok(PlanPreview {
            batch_id_short: short_batch_id(batch),
            current_depth: batch.depth,
            current_usage_pct: usage_pct,
            current_ttl_seconds: current_ttl,
            usage_threshold_pct: usage_threshold,
            ttl_threshold_seconds,
            extra_depth,
            action: PlanAction::Dilute {
                new_depth,
                post_dilute_ttl_seconds: post_dilute_ttl,
            },
            total_cost_bzz: 0.0,
            reason: format!(
                "usage above threshold ({:.0}%) — dilute",
                usage_threshold * 100.0
            ),
        }),
        (true, true) => {
            // Topup first to a TTL high enough that post-dilute we
            // still clear the threshold. Required pre-dilute TTL is
            // `ttl_threshold × dilute_factor`. Topup buys the gap.
            let target_pre_dilute_ttl = ttl_threshold_seconds.saturating_mul(dilute_factor.max(1));
            let needed = target_pre_dilute_ttl.saturating_sub(current_ttl).max(1);
            let amount = amount_for_ttl_extension(
                needed,
                &chain_state.current_price,
                GNOSIS_BLOCK_TIME_SECS,
            );
            let cost = cost_bzz(&amount, batch.depth);
            let post_dilute_ttl = (current_ttl + needed) / dilute_factor.max(1);
            Ok(PlanPreview {
                batch_id_short: short_batch_id(batch),
                current_depth: batch.depth,
                current_usage_pct: usage_pct,
                current_ttl_seconds: current_ttl,
                usage_threshold_pct: usage_threshold,
                ttl_threshold_seconds,
                extra_depth,
                action: PlanAction::TopupThenDilute {
                    topup_delta_amount_plur: amount,
                    topup_cost_bzz: cost,
                    new_depth,
                    post_dilute_ttl_seconds: post_dilute_ttl,
                },
                total_cost_bzz: cost,
                reason:
                    "usage above threshold + post-dilute TTL would fall below — topup then dilute"
                        .to_string(),
            })
        }
    }
}

/// Fractional bucket usage on `[0, 1]`. Bee's `utilization` is the
/// max-bucket count, so the meaningful denominator is
/// `2^(depth - bucket_depth)` (the per-bucket capacity).
fn stamp_usage(utilization: u32, depth: u8, bucket_depth: u8) -> f64 {
    if depth <= bucket_depth {
        return 0.0;
    }
    let denom = 1u64 << (depth - bucket_depth);
    f64::from(utilization) / (denom as f64)
}

/// Output of `:buy-suggest` — the inverse of `:buy-preview`.
/// Operator supplies a *target* (size + duration); we return the
/// minimum (depth, amount) that meets it. Capacity rounds *up* to
/// the next power-of-two depth (Bee batches are sized in
/// `2^depth × 4 KiB` increments) so the headroom is operator-
/// visible. Amount is the per-chunk PLUR that buys at least the
/// requested duration at the current chain price.
#[derive(Debug, Clone, PartialEq)]
pub struct BuySuggestion {
    pub target_bytes: u128,
    pub target_seconds: i64,
    pub depth: u8,
    pub amount_plur: BigInt,
    /// Actual capacity at the chosen depth (≥ `target_bytes`).
    pub capacity_bytes: u128,
    /// Actual TTL at the chosen amount (≥ `target_seconds`).
    pub ttl_seconds: i64,
    pub cost_bzz: f64,
}

impl BuySuggestion {
    pub fn summary(&self) -> String {
        format!(
            "buy-suggest {} / {}: depth={} amount={} PLUR/chunk → capacity {}, TTL {}, cost {:.4} BZZ",
            format_bytes(self.target_bytes),
            format_ttl_seconds(self.target_seconds),
            self.depth,
            self.amount_plur,
            format_bytes(self.capacity_bytes),
            format_ttl_seconds(self.ttl_seconds),
            self.cost_bzz,
        )
    }
}

/// Theoretical capacity in bytes for a depth, before bucket skew.
/// `2^depth × 4 KiB`.
pub fn theoretical_capacity_bytes(depth: u8) -> u128 {
    (1u128 << depth) * 4096
}

/// Convert a per-chunk PLUR amount into total BZZ paid for a batch
/// of the given depth. Same `amount × 2^depth / 1e16` formula used
/// across the ecosystem.
pub fn cost_bzz(amount_per_chunk: &BigInt, depth: u8) -> f64 {
    let total_plur: BigInt = amount_per_chunk * (BigInt::from(1u32) << depth as usize);
    total_plur.to_string().parse::<f64>().unwrap_or(0.0) / PLUR_PER_BZZ
}

/// TTL in seconds for `amount_per_chunk` PLUR at the current chain
/// price. Returns 0 if `current_price` is zero (chain hasn't loaded
/// yet — caller should fall back to "n/a").
pub fn ttl_seconds(amount_per_chunk: &BigInt, current_price: &BigInt, blocktime: i64) -> i64 {
    if current_price <= &BigInt::from(0) {
        return 0;
    }
    let ttl_blocks: BigInt = amount_per_chunk / current_price;
    let secs: BigInt = &ttl_blocks * BigInt::from(blocktime);
    secs.to_string().parse::<i64>().unwrap_or(i64::MAX)
}

/// Inverse of [`ttl_seconds`]: how much per-chunk PLUR the operator
/// must add to gain `extra_seconds` of TTL at the current price.
pub fn amount_for_ttl_extension(
    extra_seconds: i64,
    current_price: &BigInt,
    blocktime: i64,
) -> BigInt {
    if extra_seconds <= 0 || blocktime <= 0 {
        return BigInt::from(0);
    }
    let extra_blocks = BigInt::from(extra_seconds / blocktime);
    extra_blocks * current_price
}

/// Compute a topup preview against an existing batch. Reads the
/// chain price from `chain_state`; returns an `Err` summary string
/// if the chain isn't loaded yet (so the caller can surface a
/// useful command-bar message rather than silent 0s).
pub fn topup_preview(
    batch: &PostageBatch,
    delta_amount: BigInt,
    chain_state: &ChainState,
) -> Result<TopupPreview, String> {
    if chain_state.current_price <= BigInt::from(0) {
        return Err("chain price not loaded yet — try again in a moment".into());
    }
    if delta_amount <= BigInt::from(0) {
        return Err("topup amount must be a positive PLUR value".into());
    }
    let extra_ttl_seconds = ttl_seconds(
        &delta_amount,
        &chain_state.current_price,
        GNOSIS_BLOCK_TIME_SECS,
    );
    let new_ttl_seconds = batch.batch_ttl.max(0).saturating_add(extra_ttl_seconds);
    let cost = cost_bzz(&delta_amount, batch.depth);
    Ok(TopupPreview {
        batch_id_short: short_batch_id(batch),
        current_depth: batch.depth,
        current_ttl_seconds: batch.batch_ttl,
        delta_amount,
        extra_ttl_seconds,
        new_ttl_seconds,
        cost_bzz: cost,
    })
}

/// Compute a dilute preview. Bee's dilute keeps the existing PLUR
/// balance but redistributes it across more chunks: new depth must
/// be strictly greater than the current depth, and per-chunk amount
/// halves with every +1 in depth.
pub fn dilute_preview(batch: &PostageBatch, new_depth: u8) -> Result<DilutePreview, String> {
    if new_depth <= batch.depth {
        return Err(format!(
            "new depth {} must be greater than current depth {} (dilute can only raise depth)",
            new_depth, batch.depth
        ));
    }
    if new_depth > 41 {
        return Err(format!(
            "depth {new_depth} exceeds Bee's depth ceiling (41) — refusing to preview"
        ));
    }
    let delta = (new_depth - batch.depth) as u32;
    let factor = 1u128 << delta;
    let old_capacity = theoretical_capacity_bytes(batch.depth);
    let new_capacity = theoretical_capacity_bytes(new_depth);
    let old_ttl = batch.batch_ttl.max(0);
    let new_ttl = old_ttl / (factor.min(i64::MAX as u128) as i64).max(1);
    Ok(DilutePreview {
        batch_id_short: short_batch_id(batch),
        old_depth: batch.depth,
        new_depth,
        old_capacity_bytes: old_capacity,
        new_capacity_bytes: new_capacity,
        old_ttl_seconds: old_ttl,
        new_ttl_seconds: new_ttl,
    })
}

/// Compute an extend preview. Given a target TTL extension (in
/// seconds), figures out the per-chunk PLUR needed and the BZZ cost.
pub fn extend_preview(
    batch: &PostageBatch,
    extension_seconds: i64,
    chain_state: &ChainState,
) -> Result<ExtendPreview, String> {
    if extension_seconds <= 0 {
        return Err("extension must be a positive duration".into());
    }
    if chain_state.current_price <= BigInt::from(0) {
        return Err("chain price not loaded yet — try again in a moment".into());
    }
    let needed_amount = amount_for_ttl_extension(
        extension_seconds,
        &chain_state.current_price,
        GNOSIS_BLOCK_TIME_SECS,
    );
    let cost = cost_bzz(&needed_amount, batch.depth);
    let new_ttl_seconds = batch.batch_ttl.max(0).saturating_add(extension_seconds);
    Ok(ExtendPreview {
        batch_id_short: short_batch_id(batch),
        depth: batch.depth,
        current_ttl_seconds: batch.batch_ttl,
        extension_seconds,
        needed_amount_plur: needed_amount,
        cost_bzz: cost,
        new_ttl_seconds,
    })
}

/// Compute a buy preview for a hypothetical fresh batch. No batch
/// lookup needed; the operator supplies depth + per-chunk PLUR.
pub fn buy_preview(
    depth: u8,
    amount_plur: BigInt,
    chain_state: &ChainState,
) -> Result<BuyPreview, String> {
    if depth < 17 {
        return Err(format!(
            "depth {depth} is below Bee's minimum (17) — refusing to preview"
        ));
    }
    if depth > 41 {
        return Err(format!(
            "depth {depth} exceeds Bee's depth ceiling (41) — refusing to preview"
        ));
    }
    if amount_plur <= BigInt::from(0) {
        return Err("amount must be a positive PLUR value".into());
    }
    if chain_state.current_price <= BigInt::from(0) {
        return Err("chain price not loaded yet — try again in a moment".into());
    }
    let capacity_bytes = theoretical_capacity_bytes(depth);
    let ttl = ttl_seconds(
        &amount_plur,
        &chain_state.current_price,
        GNOSIS_BLOCK_TIME_SECS,
    );
    let cost = cost_bzz(&amount_plur, depth);
    Ok(BuyPreview {
        depth,
        amount_plur,
        capacity_bytes,
        ttl_seconds: ttl,
        cost_bzz: cost,
    })
}

/// Inverse of [`buy_preview`]: operator says "I want X bytes for Y
/// seconds", we return the minimum `(depth, amount)` pair that
/// covers it.
///
/// Depth rounds *up* to the next power of two so the actual
/// capacity is ≥ target_bytes (the alternative — exactly fit —
/// would silently truncate the operator's stated need). Amount
/// rounds *up* in blocks so the actual TTL is ≥ target_seconds.
///
/// Errors if the chain price isn't loaded yet or if the target
/// exceeds Bee's depth ceiling (41).
pub fn buy_suggest(
    target_bytes: u128,
    target_seconds: i64,
    chain_state: &ChainState,
) -> Result<BuySuggestion, String> {
    if target_bytes == 0 {
        return Err("target size must be positive".into());
    }
    if target_seconds <= 0 {
        return Err("target duration must be positive".into());
    }
    if chain_state.current_price <= BigInt::from(0) {
        return Err("chain price not loaded yet — try again in a moment".into());
    }

    // chunks_needed = ceil(target_bytes / 4096)
    let chunks_needed = target_bytes.div_ceil(4096);
    // depth = ceil(log2(chunks_needed)), clamped to Bee's [17, 41]
    // bounds. depth=17 is Bee's minimum useful batch size; depth>41
    // exceeds the contract's enforced ceiling.
    let raw_depth = if chunks_needed <= 1 {
        0
    } else {
        // ceil(log2(n)) — using leading_zeros for a portable answer.
        128 - (chunks_needed - 1).leading_zeros()
    };
    if raw_depth > 41 {
        return Err(format!(
            "target {} exceeds Bee's max batch capacity (depth 41 ≈ 8 PiB)",
            format_bytes(target_bytes)
        ));
    }
    let depth: u8 = raw_depth.max(17) as u8;
    let capacity_bytes = theoretical_capacity_bytes(depth);

    // ttl_blocks_needed = ceil(target_seconds / blocktime)
    let target_blocks =
        target_seconds.saturating_add(GNOSIS_BLOCK_TIME_SECS - 1) / GNOSIS_BLOCK_TIME_SECS;
    let amount = BigInt::from(target_blocks) * &chain_state.current_price;

    // Actual TTL the chosen amount yields, given ceil rounding.
    let ttl_seconds = ttl_seconds(&amount, &chain_state.current_price, GNOSIS_BLOCK_TIME_SECS);
    let cost = cost_bzz(&amount, depth);

    Ok(BuySuggestion {
        target_bytes,
        target_seconds,
        depth,
        amount_plur: amount,
        capacity_bytes,
        ttl_seconds,
        cost_bzz: cost,
    })
}

fn short_batch_id(batch: &PostageBatch) -> String {
    let hex = batch.batch_id.to_hex();
    if hex.len() > 8 {
        format!("{}…", &hex[..8])
    } else {
        hex
    }
}

/// Parse a human-readable size into bytes. Accepts plain integers
/// (`4096` = bytes), binary suffixes (`5GiB`, `2TiB`, `512MiB`),
/// and decimal suffixes (`5GB`, `1TB`, `500MB`). Single-letter
/// shorthands (`5G`, `2T`, `100M`, `4K`) default to **binary**
/// because operators reasoning about Bee's depth=2^N chunk counts
/// always think in powers of two. Suffix matching is
/// case-insensitive.
///
/// Used by `:buy-suggest` so operators can type sizes the way they
/// do in chat ("5 GiB for 30d") rather than hand-converting to
/// raw bytes.
pub fn parse_size_bytes(s: &str) -> Result<u128, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("size cannot be empty".into());
    }
    // Strip any internal whitespace between the number and the unit
    // ("5 GiB" → "5GiB") so we don't reject a perfectly clear input.
    let compact: String = s.chars().filter(|c| !c.is_whitespace()).collect();
    let (num_part, mul) = split_size(&compact)
        .ok_or_else(|| format!("invalid size {s:?} (try 5GiB, 2TiB, 500MiB, 4096)"))?;
    let n: u128 = num_part
        .parse()
        .map_err(|_| format!("invalid size {s:?} (numeric part {num_part:?} unparseable)"))?;
    if n == 0 {
        return Err("size must be positive".into());
    }
    n.checked_mul(mul).ok_or_else(|| {
        format!("size {s:?} overflowed u128 — that's larger than any plausible Bee batch")
    })
}

/// Split a compact size string into (digits, multiplier). Returns
/// `None` on unrecognised suffix.
fn split_size(s: &str) -> Option<(&str, u128)> {
    // Find first non-digit char; everything before is the number,
    // everything after (lowercased) is the unit.
    let split = s
        .char_indices()
        .find(|(_, c)| !c.is_ascii_digit())
        .map(|(i, _)| i)
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let unit_lower = unit.to_ascii_lowercase();
    let mul: u128 = match unit_lower.as_str() {
        "" | "b" => 1,
        "k" | "kib" => 1024,
        "kb" => 1_000,
        "m" | "mib" => 1024u128.pow(2),
        "mb" => 1_000u128.pow(2),
        "g" | "gib" => 1024u128.pow(3),
        "gb" => 1_000u128.pow(3),
        "t" | "tib" => 1024u128.pow(4),
        "tb" => 1_000u128.pow(4),
        "p" | "pib" => 1024u128.pow(5),
        "pb" => 1_000u128.pow(5),
        _ => return None,
    };
    Some((num, mul))
}

/// Parse a duration written like `30d` / `12h` / `90m` / `45s` /
/// plain seconds (`5000`). Used by `:extend-preview` so operators
/// don't have to convert days to seconds in their head. Rejects
/// negative or zero values; returns an actionable error otherwise.
pub fn parse_duration_seconds(s: &str) -> Result<i64, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("duration cannot be empty".into());
    }
    let (num_part, unit) = match s.chars().last() {
        Some(c) if "smhdSMHD".contains(c) => (&s[..s.len() - 1], Some(c.to_ascii_lowercase())),
        _ => (s, None),
    };
    let n: i64 = num_part
        .parse()
        .map_err(|_| format!("invalid duration {s:?} (try 30d / 12h / 90m / 45s / 5000)"))?;
    if n <= 0 {
        return Err(format!("duration must be positive, got {n}"));
    }
    let secs = match unit {
        Some('s') | None => n,
        Some('m') => n.saturating_mul(60),
        Some('h') => n.saturating_mul(3_600),
        Some('d') => n.saturating_mul(86_400),
        _ => unreachable!("unit guard above"),
    };
    Ok(secs)
}

/// Parse a per-chunk PLUR amount. Plain-integer only for now —
/// scientific notation (`1e14`) is harder to read back than to write
/// and operators copy-paste these from chain explorers anyway.
pub fn parse_plur_amount(s: &str) -> Result<BigInt, String> {
    let s = s.trim();
    if s.is_empty() {
        return Err("amount cannot be empty".into());
    }
    s.parse::<BigInt>()
        .map_err(|_| format!("invalid PLUR amount {s:?} (digits only, e.g. 100000000000)"))
}

/// Resolve a user-typed batch prefix (typically the 8 hex chars
/// shown in the S2 table) to the matching `PostageBatch`. Errors on
/// no-match or ambiguous-match so the operator doesn't preview the
/// wrong batch by accident.
pub fn match_batch_prefix<'a>(
    batches: &'a [PostageBatch],
    prefix: &str,
) -> Result<&'a PostageBatch, String> {
    let prefix = prefix.trim().trim_end_matches('…').to_ascii_lowercase();
    if prefix.is_empty() {
        return Err("batch id prefix cannot be empty".into());
    }
    let matches: Vec<&PostageBatch> = batches
        .iter()
        .filter(|b| {
            b.batch_id
                .to_hex()
                .to_ascii_lowercase()
                .starts_with(&prefix)
        })
        .collect();
    match matches.as_slice() {
        [] => Err(format!(
            "no batch matches prefix {prefix:?} (try the 8-char hex shown in S2)"
        )),
        [single] => Ok(single),
        many => Err(format!(
            "{} batches match prefix {prefix:?}: {} — type a longer prefix",
            many.len(),
            many.iter()
                .map(|b| short_batch_id(b))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_batch(amount: Option<BigInt>, depth: u8, batch_ttl: i64) -> PostageBatch {
        PostageBatch {
            batch_id: bee::swarm::BatchId::new(&[0xab; 32]).unwrap(),
            amount,
            start: 0,
            owner: String::new(),
            depth,
            bucket_depth: depth.saturating_sub(6),
            immutable: true,
            batch_ttl,
            utilization: 0,
            usable: true,
            exists: true,
            label: "test".into(),
            block_number: 0,
        }
    }

    fn chain(current_price_plur: u64) -> ChainState {
        ChainState {
            block: 100,
            chain_tip: 100,
            current_price: BigInt::from(current_price_plur),
            total_amount: BigInt::from(0),
        }
    }

    #[test]
    fn capacity_at_depth_22_is_16_gib() {
        // 2^22 × 4096 = 16 GiB exactly.
        assert_eq!(theoretical_capacity_bytes(22), 16 * 1024 * 1024 * 1024);
    }

    #[test]
    fn cost_bzz_matches_canonical_formula() {
        // amount=1e14 PLUR/chunk × 2^22 chunks / 1e16 PLUR/BZZ
        //   = 1e14 × 4_194_304 / 1e16 = 41943.04 BZZ.
        let amount = BigInt::from(100_000_000_000_000u64);
        let bzz = cost_bzz(&amount, 22);
        assert!(
            (bzz - 41943.04).abs() < 0.0001,
            "expected ~41943.04 BZZ, got {bzz}"
        );
    }

    #[test]
    fn ttl_seconds_basic() {
        // amount=1_000_000 PLUR/chunk, current_price=1 PLUR/block
        //   → ttl_blocks = 1_000_000, ttl_secs = 5_000_000.
        let secs = ttl_seconds(
            &BigInt::from(1_000_000u64),
            &BigInt::from(1u64),
            GNOSIS_BLOCK_TIME_SECS,
        );
        assert_eq!(secs, 5_000_000);
    }

    #[test]
    fn ttl_seconds_zero_price_returns_zero() {
        let secs = ttl_seconds(
            &BigInt::from(1_000_000u64),
            &BigInt::from(0u64),
            GNOSIS_BLOCK_TIME_SECS,
        );
        assert_eq!(secs, 0);
    }

    #[test]
    fn amount_for_extension_is_inverse_of_ttl() {
        // Extending by 5_000_000 seconds at price=1 PLUR/block gives
        // back 1_000_000 PLUR/chunk.
        let amt = amount_for_ttl_extension(5_000_000, &BigInt::from(1u64), GNOSIS_BLOCK_TIME_SECS);
        assert_eq!(amt, BigInt::from(1_000_000u64));
    }

    #[test]
    fn topup_preview_typical_case() {
        // depth=22, amount=delta=1e10 PLUR/chunk, price=1 PLUR/block.
        // extra_ttl = 1e10 × 5 = 5e10 seconds.
        // cost = 1e10 × 2^22 / 1e16 = 4.194 BZZ.
        let batch = make_batch(Some(BigInt::from(0)), 22, 86_400);
        let preview = topup_preview(&batch, BigInt::from(10_000_000_000u64), &chain(1)).unwrap();
        assert_eq!(preview.current_depth, 22);
        assert_eq!(preview.extra_ttl_seconds, 50_000_000_000);
        assert!((preview.cost_bzz - 4.194304).abs() < 0.0001);
        assert_eq!(preview.new_ttl_seconds, 86_400 + 50_000_000_000);
    }

    #[test]
    fn topup_preview_rejects_zero_price() {
        let batch = make_batch(None, 22, 86_400);
        let err = topup_preview(&batch, BigInt::from(1_000), &chain(0)).unwrap_err();
        assert!(err.contains("chain price"));
    }

    #[test]
    fn topup_preview_rejects_zero_delta() {
        let batch = make_batch(None, 22, 86_400);
        let err = topup_preview(&batch, BigInt::from(0), &chain(1)).unwrap_err();
        assert!(err.contains("positive PLUR"));
    }

    #[test]
    fn dilute_preview_doubles_capacity_halves_ttl() {
        // Going from depth 22 → 23 doubles capacity, halves TTL.
        let batch = make_batch(None, 22, 100_000);
        let preview = dilute_preview(&batch, 23).unwrap();
        assert_eq!(preview.old_capacity_bytes * 2, preview.new_capacity_bytes);
        assert_eq!(preview.old_ttl_seconds / 2, preview.new_ttl_seconds);
        assert!(preview.summary().contains("cost 0 BZZ"));
    }

    #[test]
    fn dilute_preview_rejects_lower_or_equal_depth() {
        let batch = make_batch(None, 22, 100_000);
        assert!(dilute_preview(&batch, 22).is_err());
        assert!(dilute_preview(&batch, 21).is_err());
    }

    #[test]
    fn dilute_preview_rejects_above_depth_ceiling() {
        let batch = make_batch(None, 22, 100_000);
        assert!(dilute_preview(&batch, 42).is_err());
    }

    #[test]
    fn extend_preview_typical_case() {
        // Extend by 5_000_000s (~58 days) at price=1, blocktime=5:
        // needed_amount = 5_000_000 / 5 × 1 = 1_000_000 PLUR/chunk.
        // depth=22 → cost = 1e6 × 2^22 / 1e16 = 4.194304e-4 BZZ.
        let batch = make_batch(None, 22, 86_400);
        let preview = extend_preview(&batch, 5_000_000, &chain(1)).unwrap();
        assert_eq!(preview.needed_amount_plur, BigInt::from(1_000_000u64));
        assert!((preview.cost_bzz - 4.194304e-4).abs() < 1e-9);
        assert_eq!(preview.new_ttl_seconds, 86_400 + 5_000_000);
    }

    #[test]
    fn extend_preview_rejects_zero_extension() {
        let batch = make_batch(None, 22, 86_400);
        assert!(extend_preview(&batch, 0, &chain(1)).is_err());
        assert!(extend_preview(&batch, -10, &chain(1)).is_err());
    }

    #[test]
    fn buy_preview_typical_case() {
        // depth=22, amount=1e14 PLUR/chunk, price=1 PLUR/block.
        // capacity = 16 GiB, ttl = 1e14 × 5 = 5e14 secs, cost = 41943.04 BZZ.
        let preview = buy_preview(22, BigInt::from(100_000_000_000_000u64), &chain(1)).unwrap();
        assert_eq!(preview.capacity_bytes, 16 * 1024 * 1024 * 1024);
        assert_eq!(preview.ttl_seconds, 500_000_000_000_000);
        assert!((preview.cost_bzz - 41943.04).abs() < 0.0001);
    }

    #[test]
    fn buy_preview_rejects_below_minimum_depth() {
        assert!(buy_preview(16, BigInt::from(100), &chain(1)).is_err());
    }

    #[test]
    fn buy_preview_rejects_above_ceiling() {
        assert!(buy_preview(42, BigInt::from(100), &chain(1)).is_err());
    }

    #[test]
    fn buy_preview_rejects_zero_amount() {
        assert!(buy_preview(22, BigInt::from(0), &chain(1)).is_err());
    }

    #[test]
    fn parse_size_plain_integer_is_bytes() {
        assert_eq!(parse_size_bytes("4096").unwrap(), 4096);
        assert!(parse_size_bytes("0").is_err());
        assert!(parse_size_bytes("").is_err());
    }

    #[test]
    fn parse_size_binary_suffixes() {
        assert_eq!(parse_size_bytes("1KiB").unwrap(), 1024);
        assert_eq!(parse_size_bytes("1MiB").unwrap(), 1024u128.pow(2));
        assert_eq!(parse_size_bytes("1GiB").unwrap(), 1024u128.pow(3));
        assert_eq!(parse_size_bytes("1TiB").unwrap(), 1024u128.pow(4));
        // Single-letter shorthand defaults to binary (operator-friendly
        // for power-of-two batch reasoning).
        assert_eq!(parse_size_bytes("1G").unwrap(), 1024u128.pow(3));
        assert_eq!(parse_size_bytes("4K").unwrap(), 4096);
    }

    #[test]
    fn parse_size_decimal_suffixes() {
        assert_eq!(parse_size_bytes("1KB").unwrap(), 1_000);
        assert_eq!(parse_size_bytes("1MB").unwrap(), 1_000_000);
        assert_eq!(parse_size_bytes("1GB").unwrap(), 1_000_000_000);
    }

    #[test]
    fn parse_size_handles_whitespace_and_case() {
        assert_eq!(parse_size_bytes(" 5 GiB ").unwrap(), 5 * 1024u128.pow(3));
        assert_eq!(parse_size_bytes("5gib").unwrap(), 5 * 1024u128.pow(3));
        assert_eq!(parse_size_bytes("2 TIB").unwrap(), 2 * 1024u128.pow(4));
    }

    #[test]
    fn parse_size_rejects_unknown_unit() {
        assert!(parse_size_bytes("5xyz").is_err());
        assert!(parse_size_bytes("abc").is_err());
    }

    #[test]
    fn buy_suggest_typical_5gib_30d() {
        // 5 GiB needs ceil(log2(5*256K)) = ceil(20.32) = 21 → 8 GiB.
        // 30d at 5s blocktime = 30*86400/5 = 518_400 blocks.
        // amount = 518_400 * 1 = 518_400 PLUR/chunk (price=1).
        // TTL at amount=518_400, price=1, blocktime=5 → 2_592_000s = 30d.
        let s = buy_suggest(5 * 1024u128.pow(3), 30 * 86_400, &chain(1)).unwrap();
        assert_eq!(s.depth, 21);
        assert_eq!(s.capacity_bytes, 8 * 1024u128.pow(3));
        assert_eq!(s.amount_plur, BigInt::from(518_400u32));
        assert_eq!(s.ttl_seconds, 30 * 86_400);
    }

    #[test]
    fn buy_suggest_4gib_exact_uses_depth_20() {
        // 4 GiB exactly = 2^20 chunks * 4096 → depth 20 fits exactly.
        let s = buy_suggest(4 * 1024u128.pow(3), 86_400, &chain(1)).unwrap();
        assert_eq!(s.depth, 20);
        assert_eq!(s.capacity_bytes, 4 * 1024u128.pow(3));
    }

    #[test]
    fn buy_suggest_tiny_target_clamps_to_min_depth_17() {
        // 1 chunk's worth → ceil(log2(1)) = 0 → clamp to 17.
        let s = buy_suggest(4096, 86_400, &chain(1)).unwrap();
        assert_eq!(s.depth, 17);
        assert!(s.capacity_bytes >= 4096);
    }

    #[test]
    fn buy_suggest_rejects_above_max_depth() {
        // depth 42 ≈ 16 PiB; explicitly refused.
        let huge = 16 * 1024u128.pow(5); // 16 PiB
        assert!(buy_suggest(huge, 86_400, &chain(1)).is_err());
    }

    #[test]
    fn buy_suggest_rounds_duration_up_in_blocks() {
        // 7 seconds at 5s blocktime → 2 blocks (ceil), not 1.
        // amount = 2 * 1 = 2; TTL = 2 * 5 = 10s ≥ 7.
        let s = buy_suggest(4096, 7, &chain(1)).unwrap();
        assert_eq!(s.amount_plur, BigInt::from(2u32));
        assert_eq!(s.ttl_seconds, 10);
    }

    #[test]
    fn buy_suggest_rejects_zero_or_negative_inputs() {
        assert!(buy_suggest(0, 86_400, &chain(1)).is_err());
        assert!(buy_suggest(4096, 0, &chain(1)).is_err());
        assert!(buy_suggest(4096, -5, &chain(1)).is_err());
    }

    #[test]
    fn buy_suggest_rejects_zero_chain_price() {
        assert!(buy_suggest(4096, 86_400, &chain(0)).is_err());
    }

    #[test]
    fn buy_suggest_summary_is_compact() {
        let s = buy_suggest(5 * 1024u128.pow(3), 30 * 86_400, &chain(1)).unwrap();
        let line = s.summary();
        assert!(line.starts_with("buy-suggest"));
        assert!(line.contains("5.0 GiB"));
        assert!(line.contains("30d  0h"));
        assert!(line.contains("depth=21"));
        assert!(!line.contains('\n'));
    }

    #[test]
    fn parse_duration_handles_units() {
        assert_eq!(parse_duration_seconds("5000").unwrap(), 5_000);
        assert_eq!(parse_duration_seconds("45s").unwrap(), 45);
        assert_eq!(parse_duration_seconds("90m").unwrap(), 5_400);
        assert_eq!(parse_duration_seconds("12h").unwrap(), 43_200);
        assert_eq!(parse_duration_seconds("30d").unwrap(), 2_592_000);
        // Trailing whitespace + uppercase unit.
        assert_eq!(parse_duration_seconds(" 7D ").unwrap(), 604_800);
    }

    #[test]
    fn parse_duration_rejects_invalid() {
        assert!(parse_duration_seconds("").is_err());
        assert!(parse_duration_seconds("abc").is_err());
        assert!(parse_duration_seconds("0d").is_err());
        assert!(parse_duration_seconds("-5h").is_err());
    }

    #[test]
    fn parse_plur_handles_large_amounts() {
        let amt = parse_plur_amount("100000000000000").unwrap();
        assert_eq!(amt, BigInt::from(100_000_000_000_000u64));
    }

    #[test]
    fn parse_plur_rejects_garbage() {
        assert!(parse_plur_amount("").is_err());
        assert!(parse_plur_amount("1e14").is_err()); // scientific not supported
        assert!(parse_plur_amount("123abc").is_err());
    }

    #[test]
    fn match_batch_prefix_unique_returns_single() {
        let b1 = make_batch_with_id([0xab; 32]);
        let b2 = make_batch_with_id([0xcd; 32]);
        let batches = vec![b1.clone(), b2.clone()];
        let m = match_batch_prefix(&batches, "abab").unwrap();
        assert_eq!(m.batch_id, b1.batch_id);
    }

    #[test]
    fn match_batch_prefix_handles_trailing_ellipsis() {
        // The S2 table renders "abababab…" — operators may copy that
        // shape verbatim. Strip the trailing ellipsis transparently.
        let b1 = make_batch_with_id([0xab; 32]);
        let batches = vec![b1.clone()];
        let m = match_batch_prefix(&batches, "abababab…").unwrap();
        assert_eq!(m.batch_id, b1.batch_id);
    }

    #[test]
    fn match_batch_prefix_ambiguous_errors_with_listing() {
        let b1 = make_batch_with_id([0xab; 32]);
        let b2 = make_batch_with_id([0xab; 32]); // identical prefix
        let batches = vec![b1, b2];
        let err = match_batch_prefix(&batches, "ab").unwrap_err();
        assert!(err.contains("match prefix"));
    }

    #[test]
    fn match_batch_prefix_no_match_errors() {
        let b1 = make_batch_with_id([0xab; 32]);
        let batches = vec![b1];
        let err = match_batch_prefix(&batches, "ff").unwrap_err();
        assert!(err.contains("no batch matches"));
    }

    fn make_batch_with_id(bytes: [u8; 32]) -> PostageBatch {
        PostageBatch {
            batch_id: bee::swarm::BatchId::new(&bytes).unwrap(),
            amount: None,
            start: 0,
            owner: String::new(),
            depth: 22,
            bucket_depth: 16,
            immutable: true,
            batch_ttl: 86_400,
            utilization: 0,
            usable: true,
            exists: true,
            label: "test".into(),
            block_number: 0,
        }
    }

    #[test]
    fn summary_strings_are_compact_and_human_readable() {
        // Smoke test that summary() produces reasonable single-line
        // output (no embedded newlines, includes the verb name).
        let batch = make_batch(None, 22, 86_400);
        let p = topup_preview(&batch, BigInt::from(10u64), &chain(1)).unwrap();
        let s = p.summary();
        assert!(s.starts_with("topup-preview"));
        assert!(!s.contains('\n'));

        let p = dilute_preview(&batch, 23).unwrap();
        let s = p.summary();
        assert!(s.starts_with("dilute-preview"));
        assert!(!s.contains('\n'));

        let p = extend_preview(&batch, 86_400, &chain(1)).unwrap();
        let s = p.summary();
        assert!(s.starts_with("extend-preview"));
        assert!(!s.contains('\n'));

        let p = buy_preview(22, BigInt::from(10_000), &chain(1)).unwrap();
        let s = p.summary();
        assert!(s.starts_with("buy-preview"));
        assert!(!s.contains('\n'));
    }

    fn mutable_batch(amount: u64, depth: u8, batch_ttl: i64, utilization: u32) -> PostageBatch {
        let mut b = make_batch(Some(BigInt::from(amount)), depth, batch_ttl);
        b.immutable = false;
        b.utilization = utilization;
        b
    }

    #[test]
    fn plan_batch_healthy_returns_no_action() {
        // depth=22, util=0%, TTL=30 days, threshold 24h: nothing to do.
        let batch = mutable_batch(1_000_000, 22, 30 * 86_400, 0);
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        assert_eq!(plan.action, PlanAction::None);
        assert_eq!(plan.total_cost_bzz, 0.0);
        assert!(plan.reason.contains("healthy"));
    }

    #[test]
    fn plan_batch_low_ttl_only_topup() {
        // depth=22, util=0%, TTL=1h. Below 24h threshold, but usage
        // is 0 so no dilute needed.
        let batch = mutable_batch(1_000_000, 22, 3600, 0);
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        match plan.action {
            PlanAction::Topup {
                ref delta_amount_plur,
                ..
            } => {
                assert!(*delta_amount_plur > BigInt::from(0));
            }
            other => panic!("expected Topup, got {other:?}"),
        }
        assert!(plan.total_cost_bzz > 0.0);
    }

    #[test]
    fn plan_batch_high_usage_only_dilute() {
        // util at 100%, but TTL very high so post-dilute (TTL/4) is
        // still way above threshold. Pure dilute.
        // bucket_depth = depth - 6 = 16; depth=22; max bucket count
        // = 2^(22-16) = 64. utilization=64 → 100% usage.
        let batch = mutable_batch(1_000_000, 22, 365 * 86_400, 64);
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        match plan.action {
            PlanAction::Dilute { new_depth, .. } => {
                assert_eq!(new_depth, 24);
            }
            other => panic!("expected Dilute, got {other:?}"),
        }
        assert_eq!(plan.total_cost_bzz, 0.0);
    }

    #[test]
    fn plan_batch_high_usage_low_ttl_topup_then_dilute() {
        // util=100% AND TTL barely above threshold — post-dilute TTL
        // would fall below, so plan is topup-then-dilute.
        let batch = mutable_batch(1_000_000, 22, 2 * 24 * 3600, 64);
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        match plan.action {
            PlanAction::TopupThenDilute {
                ref topup_delta_amount_plur,
                new_depth,
                ..
            } => {
                assert!(*topup_delta_amount_plur > BigInt::from(0));
                assert_eq!(new_depth, 24);
            }
            other => panic!("expected TopupThenDilute, got {other:?}"),
        }
        assert!(plan.total_cost_bzz > 0.0);
    }

    #[test]
    fn plan_batch_immutable_high_usage_skips_dilute() {
        let mut batch = mutable_batch(1_000_000, 22, 30 * 86_400, 64);
        batch.immutable = true;
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        // Immutable + healthy TTL → None with a reason explaining
        // why we can't act on the high usage.
        assert_eq!(plan.action, PlanAction::None);
        assert!(plan.reason.contains("immutable"));
    }

    #[test]
    fn plan_batch_rejects_out_of_range_threshold() {
        let batch = mutable_batch(1_000_000, 22, 30 * 86_400, 0);
        assert!(plan_batch(&batch, &chain(1), 1.5, DEFAULT_TTL_THRESHOLD_SECONDS, 2).is_err());
        assert!(plan_batch(&batch, &chain(1), -0.1, 86400, 2).is_err());
    }

    #[test]
    fn plan_batch_summary_is_one_line() {
        let batch = mutable_batch(1_000_000, 22, 3600, 64);
        let plan = plan_batch(
            &batch,
            &chain(1),
            DEFAULT_USAGE_THRESHOLD,
            DEFAULT_TTL_THRESHOLD_SECONDS,
            DEFAULT_EXTRA_DEPTH,
        )
        .unwrap();
        let s = plan.summary();
        assert!(s.starts_with("plan-batch"));
        assert!(!s.contains('\n'), "summary must be a single line: {s}");
    }
}
