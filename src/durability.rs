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

use crate::api::ApiClient;

/// Ceiling on how many chunks one durability-check will walk before
/// giving up. Operators with very large manifests (10⁵+ chunks) get
/// a partial answer rather than a stuck cockpit. Conservative
/// default; can be lifted via a future config knob.
const MAX_CHUNKS_PER_WALK: u64 = 10_000;

/// Outcome bucket for the running summary. We separate
/// `chunks_lost` (a 404 on `/chunks/{ref}`) from `chunks_errors`
/// (any other failure — timeout, 500, decode error) because they
/// have different operator implications: lost = the network truly
/// dropped your data; errors = something flaky that needs a retry.
#[derive(Debug, Clone)]
pub struct DurabilityResult {
    pub reference: Reference,
    pub started_at: SystemTime,
    pub duration_ms: u64,
    pub chunks_total: u64,
    pub chunks_lost: u64,
    pub chunks_errors: u64,
    /// True iff the root chunk parsed as a Mantaray manifest. When
    /// false the rest of the counts come from a single raw-chunk
    /// fetch.
    pub root_is_manifest: bool,
    /// True when we hit `MAX_CHUNKS_PER_WALK` and stopped early.
    pub truncated: bool,
}

impl DurabilityResult {
    /// All checked chunks fetched cleanly.
    pub fn is_healthy(&self) -> bool {
        self.chunks_lost == 0 && self.chunks_errors == 0
    }
    /// Summary line shown on the command-status row + S13 detail.
    pub fn summary(&self) -> String {
        let kind = if self.root_is_manifest {
            "manifest"
        } else {
            "raw chunk"
        };
        let trunc = if self.truncated { " (truncated)" } else { "" };
        if self.is_healthy() {
            format!(
                "durability-check OK in {}ms · {kind} · {} chunk{} retrievable{trunc}",
                self.duration_ms,
                self.chunks_total,
                if self.chunks_total == 1 { "" } else { "s" },
            )
        } else {
            format!(
                "durability-check UNHEALTHY in {}ms · {kind} · total {} · lost {} · errors {}{trunc}",
                self.duration_ms,
                self.chunks_total,
                self.chunks_lost,
                self.chunks_errors,
            )
        }
    }
}

/// Walk the chunk graph rooted at `reference` and report the result.
/// Times out per-chunk via reqwest's default; the surrounding `tokio`
/// task can be cancelled by dropping its handle (the Watchlist
/// screen owns the in-flight handle).
pub async fn check(api: Arc<ApiClient>, reference: Reference) -> DurabilityResult {
    let started = Instant::now();
    let started_at = SystemTime::now();
    let mut result = DurabilityResult {
        reference: reference.clone(),
        started_at,
        duration_ms: 0,
        chunks_total: 0,
        chunks_lost: 0,
        chunks_errors: 0,
        root_is_manifest: false,
        truncated: false,
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
    result.duration_ms = elapsed_ms(started);
    result
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
            root_is_manifest: true,
            truncated: false,
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
            root_is_manifest: true,
            truncated: false,
        };
        let s = r.summary();
        assert!(s.contains("UNHEALTHY"), "{s}");
        assert!(s.contains("lost 2"), "{s}");
        assert!(s.contains("errors 1"), "{s}");
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
            root_is_manifest: true,
            truncated: true,
        };
        assert!(r.summary().contains("truncated"), "{}", r.summary());
    }

    #[test]
    fn is_healthy_requires_zero_lost_and_zero_errors() {
        let mut r = DurabilityResult {
            reference: fake_ref(),
            started_at: SystemTime::now(),
            duration_ms: 1,
            chunks_total: 5,
            chunks_lost: 0,
            chunks_errors: 0,
            root_is_manifest: true,
            truncated: false,
        };
        assert!(r.is_healthy());
        r.chunks_lost = 1;
        assert!(!r.is_healthy());
        r.chunks_lost = 0;
        r.chunks_errors = 1;
        assert!(!r.is_healthy());
    }
}
