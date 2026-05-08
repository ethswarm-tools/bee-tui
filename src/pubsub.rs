//! Pubsub watch — the receiver side of v1.3's `:gsoc-mine` and
//! `:pss-target` writer verbs. PSS topic subscriptions and GSOC
//! `(owner, identifier)` subscriptions both connect to a Bee
//! WebSocket and emit message frames whenever a publisher hits the
//! corresponding `/pss/send` / SOC-upload endpoint.
//!
//! The S15 Pubsub screen renders a merged timeline of all active
//! subscriptions; this module owns the subscription metadata + the
//! [`PubsubMessage`] shape that flows between the background watcher
//! tasks and the screen via an mpsc channel in `App`.

use std::path::Path;
use std::sync::Arc;
use std::time::SystemTime;

use bee::swarm::{EthAddress, Identifier, Topic};
use tokio::sync::Mutex;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::api::ApiClient;

/// Maximum number of messages the screen retains. Older messages
/// are evicted from the front (`VecDeque`-style) so the cockpit's
/// memory stays bounded even on a noisy topic.
pub const MAX_MESSAGES: usize = 500;

/// One delivered message — what the screen renders as a row.
/// Encodes both PSS (topic-keyed) and GSOC (address-keyed) variants
/// in a single shape so the merged timeline can render them
/// together, distinguished by [`PubsubKind`] in the row glyph.
#[derive(Debug, Clone)]
pub struct PubsubMessage {
    /// Wall-clock time the cockpit received the frame from Bee
    /// (not the publisher's timestamp; PSS / GSOC frames don't
    /// carry one of their own).
    pub received_at: SystemTime,
    pub kind: PubsubKind,
    /// `topic` for PSS, the SOC address (hex) for GSOC. The screen
    /// renders this short; the detail line shows the full hex.
    pub channel: String,
    /// Raw payload bytes. Bounded by Bee's chunk-size cap (4 KiB)
    /// for both PSS and GSOC.
    pub payload: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PubsubKind {
    Pss,
    Gsoc,
}

impl PubsubKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pss => "PSS",
            Self::Gsoc => "GSOC",
        }
    }
}

/// Identity for an active subscription. Used as the key in `App`'s
/// `pubsub_subs` hashmap so re-issuing `:pubsub-pss <topic>` for an
/// already-subscribed topic is detectable + cancellable.
pub fn pss_sub_id(topic: &Topic) -> String {
    format!("pss:{}", topic.to_hex())
}

pub fn gsoc_sub_id(owner: &EthAddress, identifier: &Identifier) -> String {
    format!("gsoc:{}:{}", owner.to_hex(), identifier.to_hex())
}

/// Optional history-file writer shared by every active subscription.
/// `None` when `[pubsub].history_file` is unset; `Some(file)` when
/// the operator opted in. The writer is wrapped in a `tokio::sync::Mutex`
/// so concurrent watchers serialise their appends — JSONL stays
/// well-formed even when two subscriptions deliver simultaneously.
pub type HistoryWriter = Option<Arc<Mutex<tokio::fs::File>>>;

/// Open `path` for append (`O_CREATE | O_APPEND`), creating it if
/// it doesn't exist. Returns `Some(writer)` ready to share across
/// watchers. Errors are surfaced to the caller so cockpit startup
/// can log a clear "history file disabled: <reason>" message and
/// keep the live tail going without persistence.
pub async fn open_history_writer(path: &Path) -> Result<HistoryWriter, String> {
    let mut opts = tokio::fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        // tokio::fs::OpenOptions exposes mode() directly on Unix
        // (no OpenOptionsExt import needed). Owner-only because
        // pubsub payloads can be sensitive on shared hosts.
        opts.mode(0o600);
    }
    let file = opts
        .open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    Ok(Some(Arc::new(Mutex::new(file))))
}

/// Append one JSONL line for `msg`. Failures are logged but never
/// fatal — losing a history line shouldn't kill the live tail.
async fn append_history(writer: &HistoryWriter, msg: &PubsubMessage) {
    let Some(file) = writer.as_ref() else {
        return;
    };
    let received_unix = msg
        .received_at
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = serde_json::json!({
        "received_unix": received_unix,
        "kind": msg.kind.as_str(),
        "channel": msg.channel,
        "size": msg.payload.len(),
        "payload_hex": hex_preview(&msg.payload, msg.payload.len() * 2),
    });
    let mut bytes = match serde_json::to_vec(&line) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(target: "bee_tui::pubsub", "history serialise failed: {e}");
            return;
        }
    };
    bytes.push(b'\n');
    let mut guard = file.lock().await;
    use tokio::io::AsyncWriteExt;
    if let Err(e) = guard.write_all(&bytes).await {
        tracing::warn!(target: "bee_tui::pubsub", "history append failed: {e}");
    }
}

/// Spawn a background task that drives a PSS subscription's
/// `recv()` loop and forwards every delivered message into `tx` as
/// a [`PubsubMessage`] until `cancel` fires. Returns immediately
/// after `subscribe` succeeds; the actual recv loop runs on tokio.
/// `history` is the optional shared writer that appends every
/// frame to a JSONL file on arrival.
pub async fn spawn_pss_watcher(
    api: Arc<ApiClient>,
    topic: Topic,
    cancel: CancellationToken,
    tx: UnboundedSender<PubsubMessage>,
    history: HistoryWriter,
) -> Result<(), String> {
    let mut sub = api
        .bee()
        .pss()
        .subscribe(&topic)
        .await
        .map_err(|e| format!("/pss/subscribe failed: {e}"))?;
    let channel = topic.to_hex();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = sub.recv() => {
                    match msg {
                        Some(payload) => {
                            let m = PubsubMessage {
                                received_at: SystemTime::now(),
                                kind: PubsubKind::Pss,
                                channel: channel.clone(),
                                payload: payload.to_vec(),
                            };
                            append_history(&history, &m).await;
                            let _ = tx.send(m);
                        }
                        None => return, // ws closed by Bee
                    }
                }
                _ = cancel.cancelled() => {
                    sub.cancel();
                    return;
                }
            }
        }
    });
    Ok(())
}

/// Spawn a background task that drives a GSOC subscription's
/// `recv()` loop. Same shape as [`spawn_pss_watcher`].
pub async fn spawn_gsoc_watcher(
    api: Arc<ApiClient>,
    owner: EthAddress,
    identifier: Identifier,
    cancel: CancellationToken,
    tx: UnboundedSender<PubsubMessage>,
    history: HistoryWriter,
) -> Result<(), String> {
    let mut sub = api
        .bee()
        .gsoc()
        .subscribe(&owner, &identifier)
        .await
        .map_err(|e| format!("/gsoc/subscribe failed: {e}"))?;
    // Channel = SOC address (the value Bee uses to route).
    let channel = match bee::swarm::soc::calculate_single_owner_chunk_address(&identifier, &owner) {
        Ok(r) => r.to_hex(),
        Err(e) => return Err(format!("calculate soc address: {e}")),
    };
    tokio::spawn(async move {
        loop {
            tokio::select! {
                msg = sub.recv() => {
                    match msg {
                        Some(payload) => {
                            let m = PubsubMessage {
                                received_at: SystemTime::now(),
                                kind: PubsubKind::Gsoc,
                                channel: channel.clone(),
                                payload: payload.to_vec(),
                            };
                            append_history(&history, &m).await;
                            let _ = tx.send(m);
                        }
                        None => return,
                    }
                }
                _ = cancel.cancelled() => {
                    sub.cancel();
                    return;
                }
            }
        }
    });
    Ok(())
}

/// Convert a small byte payload to a printable-ASCII preview.
/// Replaces non-printable bytes with `.`. Caps at `cap` characters.
/// Used by the screen renderer for the inline content peek.
pub fn ascii_preview(bytes: &[u8], cap: usize) -> String {
    let mut s = String::with_capacity(cap.min(bytes.len()));
    for &b in bytes.iter().take(cap) {
        if (0x20..0x7f).contains(&b) {
            s.push(b as char);
        } else {
            s.push('.');
        }
    }
    if bytes.len() > cap {
        s.push('…');
    }
    s
}

/// Hex-prefix preview for binary payloads (when ASCII is mostly
/// dots). Caps at `cap` characters of hex (so `cap/2` bytes).
pub fn hex_preview(bytes: &[u8], cap: usize) -> String {
    let bytes_to_show = bytes.iter().take(cap / 2);
    let mut s = String::with_capacity(cap.min(bytes.len() * 2));
    for b in bytes_to_show {
        s.push_str(&format!("{b:02x}"));
    }
    if bytes.len() * 2 > cap {
        s.push('…');
    }
    s
}

/// Pick the smarter of [`ascii_preview`] / [`hex_preview`] for a
/// payload — ASCII when ≥ 75% of the bytes are printable, hex
/// otherwise. Pure for testability.
pub fn smart_preview(bytes: &[u8], cap: usize) -> String {
    if bytes.is_empty() {
        return "(empty)".to_string();
    }
    let printable = bytes.iter().filter(|&&b| (0x20..0x7f).contains(&b)).count();
    let ratio = printable as f64 / bytes.len() as f64;
    if ratio >= 0.75 {
        ascii_preview(bytes, cap)
    } else {
        hex_preview(bytes, cap)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_preview_replaces_nonprintable() {
        let p = ascii_preview(&[b'h', b'i', 0x00, b'!', 0xff], 16);
        assert_eq!(p, "hi.!.");
    }

    #[test]
    fn ascii_preview_caps_with_ellipsis() {
        let p = ascii_preview(b"abcdefghij", 5);
        assert_eq!(p, "abcde…");
    }

    #[test]
    fn hex_preview_renders_two_hex_chars_per_byte() {
        let p = hex_preview(&[0xde, 0xad, 0xbe, 0xef], 8);
        assert_eq!(p, "deadbeef");
    }

    #[test]
    fn hex_preview_caps_with_ellipsis() {
        let p = hex_preview(&[0xde, 0xad, 0xbe, 0xef], 4);
        assert_eq!(p, "dead…");
    }

    #[test]
    fn smart_preview_picks_ascii_for_text() {
        let p = smart_preview(b"hello world!", 32);
        assert_eq!(p, "hello world!");
    }

    #[test]
    fn smart_preview_picks_hex_for_binary() {
        let p = smart_preview(&[0xff, 0xfe, 0xfd, 0x00], 16);
        // Binary → hex form.
        assert_eq!(p, "fffefd00");
    }

    #[test]
    fn smart_preview_handles_empty() {
        assert_eq!(smart_preview(&[], 16), "(empty)");
    }

    #[test]
    fn pss_sub_id_uses_topic_hex() {
        let topic = Topic::from_string("test-topic");
        let id = pss_sub_id(&topic);
        assert!(id.starts_with("pss:"));
        assert_eq!(&id[4..], &topic.to_hex());
    }

    #[test]
    fn gsoc_sub_id_combines_owner_and_identifier() {
        let owner = EthAddress::from_hex("0x1234567890123456789012345678901234567890").unwrap();
        let id = Identifier::new(&[0xab; 32]).unwrap();
        let sub_id = gsoc_sub_id(&owner, &id);
        assert!(sub_id.starts_with("gsoc:"));
        assert!(sub_id.contains(&owner.to_hex()));
        assert!(sub_id.contains(&id.to_hex()));
    }
}
