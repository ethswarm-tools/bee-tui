//! `:feed-probe <owner> <topic>` — single-shot lookup of the latest
//! feed update for a `(owner, topic)` pair. The cockpit verb prints
//! a one-line summary; `--once feed-probe` emits structured JSON
//! for CI snapshot-checks of feed liveness.
//!
//! ## Topic syntax
//!
//! Two accepted forms, picked by heuristic:
//!
//! - **Literal hex**: 64 hex chars (with or without `0x`) is treated
//!   as the raw 32-byte topic.
//! - **String form**: anything else is `keccak256(utf8(s))`, mirroring
//!   bee-js `Topic.fromString` and bee-cli's `topic-from-string`
//!   convention. Operators rarely think in raw 32-byte topics; they
//!   think in `"my-app/notifications"`.
//!
//! ## Why this verb
//!
//! Feeds are a Swarm primitive bee-tui has had no surface for. The
//! cockpit ships `:gsoc-mine` and `:pss-target` for the writer side;
//! `:feed-probe` is the receiver-side smoke test. Pair with
//! `:inspect <ref>` after to verify the feed's payload is the kind
//! you expect.

use std::sync::Arc;
use std::time::SystemTime;

use bee::file::FeedUpdate;
use bee::swarm::{EthAddress, Reference, Topic};

use crate::api::ApiClient;

/// Outcome of a single feed-probe lookup. Pure data so both the
/// cockpit verb (which formats a one-line message) and the `--once`
/// verb (which emits JSON) can share the same shape.
#[derive(Debug, Clone)]
pub struct FeedProbeResult {
    /// Owner (20-byte EIP-55 checksummed for display).
    pub owner_hex: String,
    /// Topic (32-byte hex), regardless of which input form the
    /// operator used.
    pub topic_hex: String,
    /// `true` when the operator gave a string we keccak256-hashed
    /// (so the JSON output can carry both forms).
    pub topic_was_string: bool,
    /// Original string-form topic when `topic_was_string` is true.
    pub topic_string: Option<String>,
    /// Index of the latest update.
    pub index: u64,
    /// Index where the next write would land.
    pub index_next: u64,
    /// Bytes-since-Unix-epoch parsed from the chunk's timestamp
    /// prefix (first 8 bytes BE). `None` if the payload is shorter
    /// than 8 bytes (malformed).
    pub timestamp_unix: Option<u64>,
    /// Total payload bytes including the timestamp prefix.
    pub payload_bytes: usize,
    /// Parsed Swarm reference if the post-timestamp portion is
    /// exactly 32 or 64 bytes (unencrypted / encrypted reference).
    /// `None` for raw feeds whose payload isn't a Swarm reference.
    pub reference_hex: Option<String>,
}

impl FeedProbeResult {
    /// One-line operator-facing summary used by both the cockpit
    /// verb and the `--once` non-JSON output.
    pub fn summary(&self) -> String {
        let ts = self
            .timestamp_unix
            .map(|t| {
                let now = SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let age = now.saturating_sub(t);
                format!("ts={t} ({})", format_age_secs(age))
            })
            .unwrap_or_else(|| "ts=?".to_string());
        let body = match &self.reference_hex {
            Some(r) => format!("ref={}", short_hex(r, 8)),
            None => format!("payload={}B", self.payload_bytes.saturating_sub(8)),
        };
        format!(
            "feed-probe owner={} · index={} · {ts} · {body}",
            short_hex(&self.owner_hex, 8),
            self.index,
        )
    }
}

/// Convert a wall-clock `age_seconds` to `12s`/`5m`/`3h`/`6d` for
/// the summary line. Pure for testability.
pub fn format_age_secs(age: u64) -> String {
    if age < 60 {
        format!("{age}s")
    } else if age < 3600 {
        format!("{}m", age / 60)
    } else if age < 86_400 {
        format!("{}h", age / 3600)
    } else {
        format!("{}d", age / 86_400)
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

/// Parse `<owner>` and `<topic>` arg strings into bee-rs primitives.
/// Owner accepts `0x`-prefixed or bare 40-hex; topic accepts 64-hex
/// (raw) or any other string (keccak256-hashed via
/// `Topic::from_string`).
pub fn parse_args(owner_str: &str, topic_str: &str) -> Result<ParsedArgs, String> {
    let owner = EthAddress::from_hex(owner_str.trim())
        .map_err(|e| format!("bad owner {owner_str:?}: {e}"))?;
    let trimmed = topic_str.trim();
    let no_prefix = trimmed.trim_start_matches("0x");
    let (topic, was_string, original) =
        if no_prefix.len() == 64 && no_prefix.chars().all(|c| c.is_ascii_hexdigit()) {
            let t = Topic::from_hex(no_prefix)
                .map_err(|e| format!("bad topic hex {topic_str:?}: {e}"))?;
            (t, false, None)
        } else {
            let t = Topic::from_string(trimmed);
            (t, true, Some(trimmed.to_string()))
        };
    Ok(ParsedArgs {
        owner,
        topic,
        topic_was_string: was_string,
        topic_string: original,
    })
}

#[derive(Debug)]
pub struct ParsedArgs {
    pub owner: EthAddress,
    pub topic: Topic,
    pub topic_was_string: bool,
    pub topic_string: Option<String>,
}

/// Run the lookup against the Bee API and convert the response into
/// a [`FeedProbeResult`]. Errors from `fetch_latest_feed_update`
/// propagate as their stringified form.
pub async fn probe(api: Arc<ApiClient>, args: ParsedArgs) -> Result<FeedProbeResult, String> {
    let upd = api
        .bee()
        .file()
        .fetch_latest_feed_update(&args.owner, &args.topic)
        .await
        .map_err(|e| {
            format!(
                "/feeds/{}/{} failed: {e}",
                args.owner.to_hex(),
                args.topic.to_hex()
            )
        })?;
    Ok(parse_update(&args, upd))
}

/// Pure parser — exposed so unit tests can pin the timestamp /
/// reference detection without an HTTP round-trip.
pub fn parse_update(args: &ParsedArgs, upd: FeedUpdate) -> FeedProbeResult {
    let bytes = upd.payload.as_ref();
    let payload_bytes = bytes.len();
    let timestamp_unix = if payload_bytes >= 8 {
        let mut ts = [0u8; 8];
        ts.copy_from_slice(&bytes[..8]);
        Some(u64::from_be_bytes(ts))
    } else {
        None
    };
    let reference_hex = if payload_bytes == 8 + 32 || payload_bytes == 8 + 64 {
        Reference::new(&bytes[8..]).ok().map(|r| r.to_hex())
    } else {
        None
    };
    FeedProbeResult {
        owner_hex: args.owner.to_hex(),
        topic_hex: args.topic.to_hex(),
        topic_was_string: args.topic_was_string,
        topic_string: args.topic_string.clone(),
        index: upd.index,
        index_next: upd.index_next,
        timestamp_unix,
        payload_bytes,
        reference_hex,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio_util::bytes::Bytes;

    fn synth_args() -> ParsedArgs {
        let owner = EthAddress::from_hex("0x1234567890123456789012345678901234567890").unwrap();
        let topic = Topic::from_hex(&"a".repeat(64)).unwrap();
        ParsedArgs {
            owner,
            topic,
            topic_was_string: false,
            topic_string: None,
        }
    }

    #[test]
    fn parse_args_accepts_hex_topic() {
        let p = parse_args(
            "0x1234567890123456789012345678901234567890",
            &"a".repeat(64),
        )
        .expect("ok");
        assert!(!p.topic_was_string);
        assert!(p.topic_string.is_none());
    }

    #[test]
    fn parse_args_keccak256_string_topic() {
        let p = parse_args(
            "0x1234567890123456789012345678901234567890",
            "my-app/notifications",
        )
        .expect("ok");
        assert!(p.topic_was_string);
        assert_eq!(p.topic_string.as_deref(), Some("my-app/notifications"));
        // Same string two ways → same topic hex.
        let other = Topic::from_string("my-app/notifications");
        assert_eq!(p.topic.to_hex(), other.to_hex());
    }

    #[test]
    fn parse_args_rejects_bad_owner() {
        match parse_args("not-an-eth-address", "any") {
            Err(_) => {}
            Ok(_) => panic!("expected error"),
        }
    }

    #[test]
    fn parse_update_extracts_timestamp_and_reference() {
        // 8-byte BE timestamp + 32-byte reference. The payload from
        // a reference-style feed.
        let mut payload = Vec::with_capacity(40);
        payload.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        payload.extend_from_slice(&[0xab; 32]);
        let upd = FeedUpdate {
            payload: Bytes::from(payload),
            index: 7,
            index_next: 8,
        };
        let r = parse_update(&synth_args(), upd);
        assert_eq!(r.index, 7);
        assert_eq!(r.timestamp_unix, Some(1_700_000_000));
        assert_eq!(r.payload_bytes, 40);
        assert_eq!(r.reference_hex.as_deref(), Some(&*"ab".repeat(32)));
    }

    #[test]
    fn parse_update_handles_raw_feed_payload() {
        // Raw feed: payload is timestamp + arbitrary bytes that
        // aren't a Swarm reference — `reference_hex` should be None.
        let mut payload = Vec::with_capacity(20);
        payload.extend_from_slice(&1_700_000_000u64.to_be_bytes());
        payload.extend_from_slice(b"hello world!");
        let upd = FeedUpdate {
            payload: Bytes::from(payload),
            index: 0,
            index_next: 1,
        };
        let r = parse_update(&synth_args(), upd);
        assert!(r.reference_hex.is_none());
        assert_eq!(r.timestamp_unix, Some(1_700_000_000));
        assert_eq!(r.payload_bytes, 20);
    }

    #[test]
    fn format_age_secs_buckets() {
        assert_eq!(format_age_secs(0), "0s");
        assert_eq!(format_age_secs(45), "45s");
        assert_eq!(format_age_secs(120), "2m");
        assert_eq!(format_age_secs(7200), "2h");
        assert_eq!(format_age_secs(2 * 86_400), "2d");
    }

    #[test]
    fn summary_includes_short_owner_index_ref() {
        let r = FeedProbeResult {
            owner_hex: "1234567890abcdef1234567890abcdef12345678".into(),
            topic_hex: "a".repeat(64),
            topic_was_string: false,
            topic_string: None,
            index: 42,
            index_next: 43,
            timestamp_unix: Some(1_700_000_000),
            payload_bytes: 40,
            reference_hex: Some("e7f3a201".repeat(8)),
        };
        let s = r.summary();
        assert!(s.contains("index=42"), "{s}");
        assert!(s.contains("12345678"), "{s}");
        assert!(s.contains("ref=e7f3a201"), "{s}");
    }
}
