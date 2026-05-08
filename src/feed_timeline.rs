//! Feed history walker — the data model behind the S14 Feed
//! Timeline screen and the `:feed-timeline` verb. Builds on v1.5's
//! [`crate::feed_probe`]: probe gives us the latest index, then the
//! walker fetches each prior index's SOC chunk in parallel (bounded
//! concurrency) and surfaces a [`Vec<TimelineEntry>`].
//!
//! ## Why a separate module
//!
//! The fetch path can't trivially reuse [`bee::file::FileApi::fetch_latest_feed_update`]:
//! that endpoint only returns the latest. For older indexes we
//! compute the SOC chunk address ourselves via
//! [`bee::file::feed_update_chunk_reference`] and download the chunk
//! directly, then unmarshal it as a SOC to recover the
//! `timestamp || payload` body. Keeping that math in one place
//! lets both the cockpit screen and the `--once` verb share it.

use std::sync::Arc;
use std::time::SystemTime;

use bee::file::feed_update_chunk_reference;
use bee::swarm::{EthAddress, Reference, Topic, soc::unmarshal_single_owner_chunk};

use crate::api::ApiClient;

/// Default ceiling on how many indexes one walk will fetch before
/// stopping. Operators with thousands of historical updates can
/// raise this with an explicit `[N]` arg or future config knob.
pub const DEFAULT_MAX_ENTRIES: u64 = 50;

/// Cap on `[N]` — even with operator override we don't walk more
/// than this in one verb call. Above this the walk is better suited
/// to a scripted shell loop with `--once feed-probe` per index.
pub const HARD_MAX_ENTRIES: u64 = 1_000;

/// One entry in the timeline.
#[derive(Debug, Clone)]
pub struct TimelineEntry {
    pub index: u64,
    /// Unix-epoch seconds parsed from the chunk's first 8 bytes BE.
    /// `None` when the payload is malformed (< 8 bytes).
    pub timestamp_unix: Option<u64>,
    /// Total payload size including the 8-byte timestamp prefix.
    pub payload_bytes: usize,
    /// Embedded Swarm reference when the post-timestamp portion is
    /// exactly 32 or 64 bytes (unencrypted / encrypted reference).
    /// `None` for raw feeds.
    pub reference_hex: Option<String>,
    /// `None` when the chunk fetched cleanly. `Some(reason)` when it
    /// was missing (404), errored, or didn't unmarshal as a SOC.
    /// The screen renders these as a dim "[lost]" / "[error]" row so
    /// gaps in the history are visible.
    pub error: Option<String>,
}

/// The full timeline view returned to callers.
#[derive(Debug, Clone)]
pub struct Timeline {
    pub owner_hex: String,
    pub topic_hex: String,
    /// Index of the most recent update at the time of the walk.
    pub latest_index: u64,
    /// Where the next write would land (=`latest_index + 1` for
    /// sequential feeds).
    pub index_next: u64,
    /// Entries in newest-first order. Length ≤ `requested_count`.
    pub entries: Vec<TimelineEntry>,
    /// `true` when we walked the full requested range; `false` when
    /// we stopped at index 0 first.
    pub reached_requested: bool,
}

impl Timeline {
    /// Single-line summary used by `--once feed-timeline`'s text
    /// output and the cockpit verb's "complete" notice.
    pub fn summary(&self) -> String {
        let lost = self.entries.iter().filter(|e| e.error.is_some()).count();
        format!(
            "feed-timeline owner={} · {} entries · latest=idx{} · {} gaps",
            short_hex(&self.owner_hex, 8),
            self.entries.len(),
            self.latest_index,
            lost,
        )
    }
}

fn short_hex(hex: &str, len: usize) -> String {
    let s = hex.trim_start_matches("0x");
    if s.len() > len {
        format!("{}…", &s[..len])
    } else {
        s.to_string()
    }
}

/// Walk a feed's history backward from the latest index. Bounded
/// concurrency: at most [`MAX_PARALLEL`] in-flight chunk fetches at
/// a time, so a 50-entry walk never opens 50 sockets.
const MAX_PARALLEL: usize = 8;

pub async fn walk(
    api: Arc<ApiClient>,
    owner: EthAddress,
    topic: Topic,
    max_entries: u64,
) -> Result<Timeline, String> {
    // First find out where the latest write landed. The cheapest way
    // is `fetch_latest_feed_update` — same call `:feed-probe` makes.
    let latest = api
        .bee()
        .file()
        .fetch_latest_feed_update(&owner, &topic)
        .await
        .map_err(|e| format!("/feeds/{}/{} failed: {e}", owner.to_hex(), topic.to_hex(),))?;

    let max_entries = max_entries.clamp(1, HARD_MAX_ENTRIES);
    let cap = (latest.index + 1).min(max_entries);
    let start = latest.index + 1 - cap; // walk inclusive of `latest.index` back to `start`
    let reached_requested = cap >= max_entries || start == 0;

    let mut entries: Vec<TimelineEntry> = Vec::with_capacity(cap as usize);
    let mut idx = latest.index + 1;
    while idx > start {
        // Fan out up to MAX_PARALLEL fetches at a time.
        let batch_size = (idx - start).min(MAX_PARALLEL as u64);
        let batch_indexes: Vec<u64> = (idx - batch_size..idx).rev().collect();
        let mut futs = Vec::with_capacity(batch_indexes.len());
        for i in &batch_indexes {
            let api_c = api.clone();
            let i = *i;
            futs.push(async move { fetch_one(api_c, owner, topic, i).await });
        }
        let results = futures::future::join_all(futs).await;
        for r in results {
            entries.push(r);
        }
        idx -= batch_size;
    }

    Ok(Timeline {
        owner_hex: owner.to_hex(),
        topic_hex: topic.to_hex(),
        latest_index: latest.index,
        index_next: latest.index_next,
        entries,
        reached_requested,
    })
}

async fn fetch_one(
    api: Arc<ApiClient>,
    owner: EthAddress,
    topic: Topic,
    index: u64,
) -> TimelineEntry {
    let soc_ref = match feed_update_chunk_reference(&owner, &topic, index) {
        Ok(r) => r,
        Err(e) => {
            return TimelineEntry {
                index,
                timestamp_unix: None,
                payload_bytes: 0,
                reference_hex: None,
                error: Some(format!("ref calc: {e}")),
            };
        }
    };
    let bytes = match api.bee().file().download_chunk(&soc_ref, None).await {
        Ok(b) => b,
        Err(e) => {
            let s = e.to_string();
            let kind = if s.contains("404") { "lost" } else { "error" };
            return TimelineEntry {
                index,
                timestamp_unix: None,
                payload_bytes: 0,
                reference_hex: None,
                error: Some(format!("{kind}: {e}")),
            };
        }
    };
    parse_soc_bytes(&bytes, &soc_ref, index)
}

/// Pure parser — exposed for unit tests. Takes the raw SOC chunk
/// bytes and the expected SOC address, returns a [`TimelineEntry`]
/// with `error=None` when the unmarshal + body shape are clean.
pub fn parse_soc_bytes(bytes: &[u8], expected: &Reference, index: u64) -> TimelineEntry {
    let soc = match unmarshal_single_owner_chunk(bytes, expected) {
        Ok(s) => s,
        Err(e) => {
            return TimelineEntry {
                index,
                timestamp_unix: None,
                payload_bytes: 0,
                reference_hex: None,
                error: Some(format!("soc parse: {e}")),
            };
        }
    };
    let payload = soc.payload.as_slice();
    let payload_bytes = payload.len();
    let timestamp_unix = if payload_bytes >= 8 {
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&payload[..8]);
        Some(u64::from_be_bytes(ts))
    } else {
        None
    };
    let reference_hex = if payload_bytes == 8 + 32 || payload_bytes == 8 + 64 {
        Reference::new(&payload[8..]).ok().map(|r| r.to_hex())
    } else {
        None
    };
    TimelineEntry {
        index,
        timestamp_unix,
        payload_bytes,
        reference_hex,
        error: None,
    }
}

/// Wall-clock age of a unix timestamp, formatted for display.
/// Re-exposed here so the screen renderer doesn't have to import
/// it from `feed_probe`.
pub fn format_age_secs(age: u64) -> String {
    crate::feed_probe::format_age_secs(age)
}

/// Helper used by the cockpit verb to map the [`TimelineEntry`]
/// list into rendered table rows. Pure for testability.
pub fn render_table(timeline: &Timeline) -> Vec<String> {
    let now = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mut out = Vec::with_capacity(timeline.entries.len() + 1);
    out.push(format!(
        "  {:>6}  {:>10}  {:>4}  {:<8}  {}",
        "INDEX", "AGE", "SIZE", "TYPE", "REF / ERROR",
    ));
    for e in &timeline.entries {
        let age = e
            .timestamp_unix
            .map(|t| format_age_secs(now.saturating_sub(t)))
            .unwrap_or_else(|| "—".to_string());
        let body = match (&e.error, &e.reference_hex) {
            (Some(err), _) => format!("[{err}]"),
            (_, Some(r)) => short_hex(r, 12).to_string(),
            (_, None) => format!("payload {}B", e.payload_bytes.saturating_sub(8)),
        };
        let kind = if e.error.is_some() {
            "miss"
        } else if e.reference_hex.is_some() {
            "ref"
        } else {
            "raw"
        };
        out.push(format!(
            "  {:>6}  {:>10}  {:>4}  {:<8}  {}",
            e.index, age, e.payload_bytes, kind, body,
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_ref() -> Reference {
        Reference::from_hex(&"a".repeat(64)).unwrap()
    }

    #[test]
    fn parse_soc_bytes_returns_error_on_short_input() {
        let r = parse_soc_bytes(&[0u8; 16], &fake_ref(), 7);
        assert!(r.error.is_some(), "{r:?}");
        assert_eq!(r.index, 7);
    }

    #[test]
    fn timeline_summary_includes_index_and_gap_count() {
        let t = Timeline {
            owner_hex: "1234567890abcdef".repeat(2),
            topic_hex: "a".repeat(64),
            latest_index: 10,
            index_next: 11,
            entries: vec![
                TimelineEntry {
                    index: 10,
                    timestamp_unix: Some(1_700_000_000),
                    payload_bytes: 40,
                    reference_hex: Some("ab".repeat(32)),
                    error: None,
                },
                TimelineEntry {
                    index: 9,
                    timestamp_unix: None,
                    payload_bytes: 0,
                    reference_hex: None,
                    error: Some("lost: 404".into()),
                },
            ],
            reached_requested: true,
        };
        let s = t.summary();
        assert!(s.contains("idx10"), "{s}");
        assert!(s.contains("1 gaps"), "{s}");
        assert!(s.contains("2 entries"), "{s}");
    }

    #[test]
    fn render_table_prints_one_row_per_entry_plus_header() {
        let t = Timeline {
            owner_hex: "0xowner".into(),
            topic_hex: "topic".into(),
            latest_index: 2,
            index_next: 3,
            entries: vec![
                TimelineEntry {
                    index: 2,
                    timestamp_unix: Some(1_700_000_000),
                    payload_bytes: 40,
                    reference_hex: Some("c".repeat(64)),
                    error: None,
                },
                TimelineEntry {
                    index: 1,
                    timestamp_unix: Some(1_699_900_000),
                    payload_bytes: 18,
                    reference_hex: None,
                    error: None,
                },
                TimelineEntry {
                    index: 0,
                    timestamp_unix: None,
                    payload_bytes: 0,
                    reference_hex: None,
                    error: Some("lost: 404".into()),
                },
            ],
            reached_requested: true,
        };
        let rows = render_table(&t);
        assert_eq!(rows.len(), 4); // header + 3 entries
        assert!(rows[0].contains("INDEX"), "{}", rows[0]);
        assert!(rows[1].contains("ref"), "{}", rows[1]);
        assert!(rows[2].contains("raw"), "{}", rows[2]);
        assert!(rows[3].contains("miss"), "{}", rows[3]);
        assert!(rows[3].contains("lost: 404"), "{}", rows[3]);
    }
}
