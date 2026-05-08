//! `:durability-check <ref>` — the operator-facing answer to the
//! single most-feared question: "is my data still alive?"
//!
//! The check walks the chunk graph rooted at `<ref>`:
//!
//! * Fetches the root chunk via `/chunks/{ref}`.
//! * If the root parses as a Mantaray manifest, recursively fetches
//!   each fork's `self_address`. Forks with `target_address` that
//!   isn't NULL are counted as leaves but their target's BMT tree is
//!   NOT walked (that's a v1.4 follow-up — bee-rs would need to
//!   stream chunks through the file chunker for a complete answer).
//! * If the root doesn't parse as a manifest, the single-chunk fetch
//!   IS the durability answer.
//!
//! Result is a [`DurabilityResult`] with `(chunks_total, chunks_lost,
//! chunks_errors)`. The S13 Watchlist screen records each invocation
//! and surfaces the running history; `cmd_status_tx` carries the
//! one-line summary back to the command bar.
//!
//! Mirrors beekeeper's `pkg/check/datadurability` but for one
//! operator's local node + one reference, without the cluster
//! orchestration.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use bee::manifest::{MantarayNode, unmarshal};
use bee::swarm::Reference;
use bee::swarm::bmt::calculate_chunk_address;

use crate::api::ApiClient;

/// Ceiling on how many chunks one durability-check will walk before
/// giving up. Operators with very large manifests (10⁵+ chunks) get
/// a partial answer rather than a stuck cockpit. Conservative
/// default; can be lifted via a future config knob.
const MAX_CHUNKS_PER_WALK: u64 = 10_000;

/// Outcome bucket for the running summary. We separate
/// `chunks_lost` (a 404 on `/chunks/{ref}`) from `chunks_errors`
/// (any other failure — timeout, 500, decode error) and from
/// `chunks_corrupt` (BMT hash of the returned content doesn't
/// match the requested reference). They have different operator
/// implications: lost = the network truly dropped your data;
/// errors = something flaky that needs a retry; corrupt = a peer
/// or local store returned different bytes than the address asked
/// for (bit-rot, swap-corrupted on-disk chunk, hostile peer).
#[derive(Debug, Clone)]
pub struct DurabilityResult {
    pub reference: Reference,
    pub started_at: SystemTime,
    pub duration_ms: u64,
    pub chunks_total: u64,
    pub chunks_lost: u64,
    pub chunks_errors: u64,
    /// Count of chunks the network returned but whose content
    /// didn't BMT-hash to the requested reference. Populated only
    /// when `bmt_verify` was on — `0` otherwise (and the operator
    /// can't tell from the count alone whether 0 means "verified
    /// clean" or "verification skipped"; check `bmt_verified`).
    pub chunks_corrupt: u64,
    /// True iff the root chunk parsed as a Mantaray manifest. When
    /// false the rest of the counts come from a single raw-chunk
    /// fetch.
    pub root_is_manifest: bool,
    /// True when we hit `MAX_CHUNKS_PER_WALK` and stopped early.
    pub truncated: bool,
    /// True when each fetched chunk had its content BMT-hashed and
    /// compared against the requested reference. Default `true` for
    /// new walks; old `DurabilityResult` records persisted to disk
    /// before v1.5 deserialise as `false` (no `chunks_corrupt`
    /// information available).
    pub bmt_verified: bool,
    /// Independent network-side answer from a swarmscan-style
    /// indexer probe. `Some(true)` = indexer sees the ref;
    /// `Some(false)` = indexer returned 404; `None` = probe was
    /// skipped (config off) or errored (timeout, non-200/404).
    pub swarmscan_seen: Option<bool>,
}

impl DurabilityResult {
    /// All checked chunks fetched cleanly + BMT-verified.
    pub fn is_healthy(&self) -> bool {
        self.chunks_lost == 0 && self.chunks_errors == 0 && self.chunks_corrupt == 0
    }
    /// Summary line shown on the command-status row + S13 detail.
    pub fn summary(&self) -> String {
        let kind = if self.root_is_manifest {
            "manifest"
        } else {
            "raw chunk"
        };
        let trunc = if self.truncated { " (truncated)" } else { "" };
        let verify = if self.bmt_verified { " · BMT" } else { "" };
        let swarmscan = match self.swarmscan_seen {
            Some(true) => " · swarmscan: seen",
            Some(false) => " · swarmscan: NOT seen",
            None => "",
        };
        if self.is_healthy() {
            format!(
                "durability-check OK in {}ms · {kind} · {} chunk{} retrievable{verify}{swarmscan}{trunc}",
                self.duration_ms,
                self.chunks_total,
                if self.chunks_total == 1 { "" } else { "s" },
            )
        } else {
            format!(
                "durability-check UNHEALTHY in {}ms · {kind} · total {} · lost {} · errors {} · corrupt {}{swarmscan}{trunc}",
                self.duration_ms,
                self.chunks_total,
                self.chunks_lost,
                self.chunks_errors,
                self.chunks_corrupt,
            )
        }
    }
}

/// Walk the chunk graph rooted at `reference` and report the result.
/// Times out per-chunk via reqwest's default; the surrounding `tokio`
/// task can be cancelled by dropping its handle (the Watchlist
/// screen owns the in-flight handle). BMT verification on by
/// default; swarmscan probe off — see [`check_with_options`].
pub async fn check(api: Arc<ApiClient>, reference: Reference) -> DurabilityResult {
    check_with_options(api, reference, CheckOptions::default()).await
}

/// Knobs for the durability walk.
#[derive(Debug, Clone)]
pub struct CheckOptions {
    /// When `true`, every fetched chunk's content is BMT-hashed
    /// and compared against the requested reference. Mismatches
    /// land in `chunks_corrupt` (separate from `chunks_lost` /
    /// `chunks_errors`). Default on for new callers — the cost is
    /// one keccak per chunk and the correctness gain is high.
    pub bmt_verify: bool,
    /// When `Some(url_template)`, after the local walk completes
    /// the cockpit hits the indexer URL (replacing `{ref}` with
    /// the hex-encoded reference) and records the outcome on
    /// `DurabilityResult.swarmscan_seen`. `None` skips the probe.
    /// Templated so swarmscan API URL changes (or operators using
    /// a different indexer) don't require a code change.
    pub swarmscan_url: Option<String>,
}

impl Default for CheckOptions {
    fn default() -> Self {
        Self {
            bmt_verify: true,
            swarmscan_url: None,
        }
    }
}

/// `check` with explicit options. Exposed so a future
/// `[durability].bmt_verify = false` config knob (or a CLI flag)
/// can opt out for very large walks where the keccak cost adds up.
pub async fn check_with_options(
    api: Arc<ApiClient>,
    reference: Reference,
    opts: CheckOptions,
) -> DurabilityResult {
    let started = Instant::now();
    let started_at = SystemTime::now();
    let mut result = DurabilityResult {
        reference: reference.clone(),
        started_at,
        duration_ms: 0,
        chunks_total: 0,
        chunks_lost: 0,
        chunks_errors: 0,
        chunks_corrupt: 0,
        root_is_manifest: false,
        truncated: false,
        bmt_verified: opts.bmt_verify,
        swarmscan_seen: None,
    };

    // Root fetch.
    let root_bytes = match api.bee().file().download_chunk(&reference, None).await {
        Ok(b) => b,
        Err(e) => {
            // Distinguish 404 (chunk genuinely not found) from other
            // failures by looking at the error string. bee-rs doesn't
            // expose a structured-error path here; we lean on the
            // text format the api client emits.
            let s = e.to_string();
            if s.contains("404") {
                result.chunks_lost = 1;
            } else {
                result.chunks_errors = 1;
            }
            result.chunks_total = 1;
            result.duration_ms = elapsed_ms(started);
            return result;
        }
    };
    result.chunks_total = 1;
    if opts.bmt_verify && !bmt_matches(&root_bytes, reference.as_bytes()) {
        // Root content doesn't hash to the requested reference —
        // count as corrupt, but still try to parse as a manifest
        // (operator gets a more useful "what was retrieved looked
        // like a manifest, but the bytes were wrong" signal).
        result.chunks_corrupt += 1;
    }

    // Try to parse as manifest. If not, we're done — single chunk
    // fetch was the answer.
    let root_node = match unmarshal(&root_bytes, reference.as_bytes()) {
        Ok(n) => n,
        Err(_) => {
            result.duration_ms = elapsed_ms(started);
            return result;
        }
    };
    result.root_is_manifest = true;

    // BFS over fork tree. Track visited self-addresses to short-circuit
    // cycles (shouldn't happen in a real manifest but cheap insurance).
    let mut visited: HashSet<[u8; 32]> = HashSet::new();
    let mut queue: Vec<MantarayNode> = vec![root_node];

    while let Some(node) = queue.pop() {
        for fork in node.forks.values() {
            let Some(addr) = fork.node.self_address else {
                continue;
            };
            if !visited.insert(addr) {
                continue;
            }
            if result.chunks_total >= MAX_CHUNKS_PER_WALK {
                result.truncated = true;
                result.duration_ms = elapsed_ms(started);
                return result;
            }
            result.chunks_total += 1;
            let child_ref = match Reference::new(&addr) {
                Ok(r) => r,
                Err(_) => {
                    result.chunks_errors += 1;
                    continue;
                }
            };
            match api.bee().file().download_chunk(&child_ref, None).await {
                Ok(child_bytes) => {
                    if opts.bmt_verify && !bmt_matches(&child_bytes, child_ref.as_bytes()) {
                        // Don't descend into corrupt chunks — their
                        // unmarshal output is untrustworthy.
                        result.chunks_corrupt += 1;
                        continue;
                    }
                    // Try to keep walking — if this fork is itself a
                    // sub-manifest its forks reach further leaves.
                    if let Ok(child_node) = unmarshal(&child_bytes, child_ref.as_bytes()) {
                        queue.push(child_node);
                    }
                }
                Err(e) => {
                    if e.to_string().contains("404") {
                        result.chunks_lost += 1;
                    } else {
                        result.chunks_errors += 1;
                    }
                }
            }
        }
    }
    // Optional swarmscan cross-check. The probe URL is templated
    // so the operator can point at any indexer with a similar
    // shape (200 = seen, 404 = not seen).
    if let Some(template) = opts.swarmscan_url.as_deref() {
        result.swarmscan_seen = swarmscan_probe(template, &reference).await;
    }
    result.duration_ms = elapsed_ms(started);
    result
}

/// HTTP GET against the swarmscan-style probe URL.
/// `Some(true)` on 200, `Some(false)` on 404, `None` on anything
/// else (timeout, 5xx, DNS failure, …) — the operator's S13 row
/// then renders "?" rather than a misleading boolean.
async fn swarmscan_probe(url_template: &str, reference: &Reference) -> Option<bool> {
    let url = url_template.replace("{ref}", &reference.to_hex());
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .user_agent(concat!("bee-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .ok()?;
    match client.get(&url).send().await {
        Ok(resp) => match resp.status().as_u16() {
            200 => Some(true),
            404 => Some(false),
            _ => None,
        },
        Err(_) => None,
    }
}

/// True when `bytes` BMT-hashes to `expected`. Returns `false` on
/// any error (e.g. payload exceeds `CHUNK_SIZE`) — caller treats
/// that as "didn't verify cleanly", which lands in `chunks_corrupt`.
fn bmt_matches(bytes: &[u8], expected: &[u8]) -> bool {
    match calculate_chunk_address(bytes) {
        Ok(a) => a.as_slice() == expected,
        Err(_) => false,
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    let d: Duration = started.elapsed();
    d.as_millis().min(u128::from(u64::MAX)) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_ref() -> Reference {
        Reference::from_hex(&"a".repeat(64)).unwrap()
    }

    #[test]
    fn summary_renders_healthy_message() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 123,
            chunks_total: 4,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        let s = r.summary();
        assert!(s.contains("OK"), "{s}");
        assert!(s.contains("4 chunks retrievable"), "{s}");
        assert!(s.contains("manifest"), "{s}");
    }

    #[test]
    fn summary_renders_unhealthy_breakdown() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 990,
            chunks_total: 8,
            chunks_lost: 2,
            chunks_errors: 1,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        let s = r.summary();
        assert!(s.contains("UNHEALTHY"), "{s}");
        assert!(s.contains("lost 2"), "{s}");
        assert!(s.contains("errors 1"), "{s}");
    }

    #[test]
    fn summary_includes_corrupt_when_bmt_finds_mismatch() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 100,
            chunks_total: 5,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 2,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        let s = r.summary();
        assert!(!r.is_healthy());
        assert!(s.contains("UNHEALTHY"), "{s}");
        assert!(s.contains("corrupt 2"), "{s}");
    }

    #[test]
    fn summary_includes_bmt_marker_when_verified() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 100,
            chunks_total: 3,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        assert!(r.summary().contains("BMT"), "{}", r.summary());
    }

    #[test]
    fn summary_omits_bmt_marker_when_skipped() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 100,
            chunks_total: 3,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: false,
            swarmscan_seen: None,
        };
        assert!(!r.summary().contains("BMT"), "{}", r.summary());
    }

    #[test]
    fn truncated_flag_surfaces_in_summary() {
        let r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 1,
            chunks_total: 10_000,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: true,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        assert!(r.summary().contains("truncated"), "{}", r.summary());
    }

    #[test]
    fn is_healthy_requires_zero_lost_errors_and_corrupt() {
        let mut r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 1,
            chunks_total: 5,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: None,
        };
        assert!(r.is_healthy());
        r.chunks_lost = 1;
        assert!(!r.is_healthy());
        r.chunks_lost = 0;
        r.chunks_errors = 1;
        assert!(!r.is_healthy());
        r.chunks_errors = 0;
        r.chunks_corrupt = 1;
        assert!(!r.is_healthy());
    }

    #[test]
    fn summary_includes_swarmscan_seen() {
        let mut r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 100,
            chunks_total: 3,
            chunks_lost: 0,
            chunks_errors: 0,
            chunks_corrupt: 0,
            root_is_manifest: true,
            truncated: false,
            bmt_verified: true,
            swarmscan_seen: Some(true),
        };
        assert!(r.summary().contains("swarmscan: seen"), "{}", r.summary());

        r.swarmscan_seen = Some(false);
        // A "NOT seen" answer doesn't change is_healthy() — it's a
        // separate independent signal — but the summary surfaces it.
        assert!(
            r.summary().contains("swarmscan: NOT seen"),
            "{}",
            r.summary(),
        );

        r.swarmscan_seen = None;
        assert!(!r.summary().contains("swarmscan"), "{}", r.summary());
    }

    #[test]
    fn bmt_matches_verifies_real_chunk() {
        // Build a span+payload pair; BMT-hash it; assert
        // bmt_matches() agrees on the same input + the chunk's
        // computed address. This guards against accidentally
        // breaking the calculate_chunk_address contract from
        // bee-rs without us noticing — the durability walk's
        // correctness depends on this round-trip.
        use bee::swarm::bmt::calculate_chunk_address;
        let payload = b"some chunk content".to_vec();
        let span_len = (payload.len() as u64).to_le_bytes();
        let mut bytes = Vec::with_capacity(8 + payload.len());
        bytes.extend_from_slice(&span_len);
        bytes.extend_from_slice(&payload);
        let addr = calculate_chunk_address(&bytes).expect("hash ok");
        assert!(bmt_matches(&bytes, addr.as_slice()));

        // Flip one byte → no longer matches.
        let mut tampered = bytes.clone();
        tampered[10] ^= 0xff;
        assert!(!bmt_matches(&tampered, addr.as_slice()));
    }
}
