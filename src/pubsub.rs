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

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

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

/// State backing the optional pubsub-history JSONL file. Tracks the
/// current write offset so the writer can rotate at a configured
/// size threshold without an extra `metadata()` syscall per append.
/// Held inside an `Arc<Mutex<_>>` so every watcher task shares the
/// same file handle + offset and serialises through the mutex.
pub struct HistoryFile {
    file: tokio::fs::File,
    path: PathBuf,
    bytes_written: u64,
    /// Rotation threshold. When `bytes_written >= rotate_size_bytes`
    /// after an append, the file is rolled over (`path.1` ←
    /// `path`). Zero disables rotation (file grows unbounded).
    rotate_size_bytes: u64,
    /// How many rotated files (`.1` .. `.N`) to retain. Older
    /// rotations are unlinked. Zero disables rotation entirely
    /// (any rotation would discard data).
    keep_files: u32,
}

/// Optional history-file handle shared by every active subscription.
/// `None` when `[pubsub].history_file` is unset; `Some(handle)` when
/// the operator opted in. JSONL appends and rotation are serialised
/// through the mutex so concurrent watchers don't tear lines.
pub type HistoryWriter = Option<Arc<Mutex<HistoryFile>>>;

/// Open `path` for append (`O_CREATE | O_APPEND`), creating it if
/// it doesn't exist. `rotate_size_bytes` and `keep_files` configure
/// rotation: when a fresh append makes the file cross the size
/// threshold, the active file is renamed to `path.1` (older
/// rotations shift to `.2`, `.3`, ..., `.keep_files`; oldest is
/// dropped) and a new empty file takes its place. Pass
/// `rotate_size_bytes = 0` to disable rotation. Errors are surfaced
/// to the caller so cockpit startup can log a clear "history file
/// disabled: <reason>" message and keep the live tail going.
pub async fn open_history_writer(
    path: &Path,
    rotate_size_bytes: u64,
    keep_files: u32,
) -> Result<HistoryWriter, String> {
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
    // Seed the byte counter from the file's current size — re-opening
    // an existing JSONL must not reset rotation tracking back to zero.
    let bytes_written = file.metadata().await.map(|m| m.len()).unwrap_or(0);
    Ok(Some(Arc::new(Mutex::new(HistoryFile {
        file,
        path: path.to_path_buf(),
        bytes_written,
        rotate_size_bytes,
        keep_files,
    }))))
}

/// Append one JSONL line for `msg`. Failures are logged but never
/// fatal — losing a history line shouldn't kill the live tail.
/// Rotates the file in place once `bytes_written` crosses the
/// configured threshold (post-append, so the rotated file may be
/// slightly above the threshold by one line).
async fn append_history(writer: &HistoryWriter, msg: &PubsubMessage) {
    let Some(handle) = writer.as_ref() else {
        return;
    };
    let received_unix = msg
        .received_at
        .duration_since(UNIX_EPOCH)
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
    let mut guard = handle.lock().await;
    use tokio::io::AsyncWriteExt;
    if let Err(e) = guard.file.write_all(&bytes).await {
        tracing::warn!(target: "bee_tui::pubsub", "history append failed: {e}");
        return;
    }
    guard.bytes_written += bytes.len() as u64;
    if guard.rotate_size_bytes > 0
        && guard.keep_files > 0
        && guard.bytes_written >= guard.rotate_size_bytes
    {
        if let Err(e) = rotate_history(&mut guard).await {
            tracing::warn!(target: "bee_tui::pubsub", "history rotate failed: {e}");
        }
    }
}

/// Roll the active history file: rename `path.{N-1}` → `path.{N}`
/// in descending order (dropping `path.{keep_files}`), then rename
/// `path` → `path.1`, then re-open `path` fresh. Holds the mutex
/// guard so no concurrent appends race with the rename.
async fn rotate_history(guard: &mut HistoryFile) -> Result<(), String> {
    use tokio::fs;
    use tokio::io::AsyncWriteExt;

    // Flush + close the current file so the rename observes a fully
    // persisted file. tokio::fs::File doesn't expose explicit close;
    // dropping the handle is enough, but we want to keep `guard.file`
    // populated for type-safety, so we replace it after opening the
    // new one (below).
    if let Err(e) = guard.file.flush().await {
        return Err(format!("flush before rotate: {e}"));
    }

    let base = guard.path.clone();
    let keep = guard.keep_files;

    // Shift rotations: i = keep..=2 → rename .{i-1} → .{i}.
    // Skip missing files (an empty rotation slot is normal on the
    // first rotation).
    for i in (2..=keep).rev() {
        let from = rotation_path(&base, i - 1);
        let to = rotation_path(&base, i);
        match fs::rename(&from, &to).await {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => {
                return Err(format!(
                    "rename {} -> {}: {e}",
                    from.display(),
                    to.display()
                ));
            }
        }
    }
    // Rename current → .1.
    let dot1 = rotation_path(&base, 1);
    fs::rename(&base, &dot1)
        .await
        .map_err(|e| format!("rename {} -> {}: {e}", base.display(), dot1.display()))?;

    // Open a fresh active file with the same options as
    // open_history_writer.
    let mut opts = fs::OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        opts.mode(0o600);
    }
    let new_file = opts
        .open(&base)
        .await
        .map_err(|e| format!("re-open {}: {e}", base.display()))?;
    guard.file = new_file;
    guard.bytes_written = 0;
    Ok(())
}

fn rotation_path(base: &Path, n: u32) -> PathBuf {
    // Append `.N` to the file name (preserves any existing extension).
    let mut s = base.as_os_str().to_os_string();
    s.push(format!(".{n}"));
    PathBuf::from(s)
}

/// Read a previously-written pubsub history file back into a vector
/// of [`PubsubMessage`]s for replay onto the S15 timeline. Lines
/// that fail to parse are logged + skipped so a single corrupt line
/// doesn't kill the whole replay. Caps the return at [`MAX_MESSAGES`]
/// (the most recent N lines), since the screen can only render that
/// many anyway. Returned in chronological order (oldest → newest).
pub async fn replay_history_file(path: &Path) -> Result<Vec<PubsubMessage>, String> {
    use tokio::io::{AsyncBufReadExt, BufReader};
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|e| format!("open {}: {e}", path.display()))?;
    let mut reader = BufReader::new(file).lines();
    let mut buf: VecDeque<PubsubMessage> = VecDeque::with_capacity(MAX_MESSAGES);
    let mut total_lines: u64 = 0;
    let mut bad_lines: u64 = 0;
    loop {
        let line = match reader.next_line().await {
            Ok(Some(l)) => l,
            Ok(None) => break,
            Err(e) => return Err(format!("read {}: {e}", path.display())),
        };
        if line.trim().is_empty() {
            continue;
        }
        total_lines += 1;
        match parse_history_line(&line) {
            Ok(m) => {
                if buf.len() == MAX_MESSAGES {
                    buf.pop_front();
                }
                buf.push_back(m);
            }
            Err(e) => {
                bad_lines += 1;
                tracing::warn!(target: "bee_tui::pubsub", "replay: skip bad line: {e}");
            }
        }
    }
    if bad_lines > 0 {
        tracing::info!(
            target: "bee_tui::pubsub",
            "replay: parsed {ok}/{total} lines ({bad} skipped)",
            ok = total_lines - bad_lines,
            total = total_lines,
            bad = bad_lines,
        );
    }
    Ok(buf.into_iter().collect())
}

/// Parse one JSONL history line into a [`PubsubMessage`]. Public
/// only for tests; the writer's format is the contract.
pub fn parse_history_line(line: &str) -> Result<PubsubMessage, String> {
    let v: serde_json::Value = serde_json::from_str(line).map_err(|e| format!("json: {e}"))?;
    let received_unix = v
        .get("received_unix")
        .and_then(|x| x.as_u64())
        .ok_or_else(|| "missing received_unix".to_string())?;
    let kind_str = v
        .get("kind")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing kind".to_string())?;
    let kind = match kind_str {
        "PSS" => PubsubKind::Pss,
        "GSOC" => PubsubKind::Gsoc,
        other => return Err(format!("bad kind {other:?}")),
    };
    let channel = v
        .get("channel")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing channel".to_string())?
        .to_string();
    let payload_hex = v
        .get("payload_hex")
        .and_then(|x| x.as_str())
        .ok_or_else(|| "missing payload_hex".to_string())?;
    let payload = decode_hex(payload_hex.trim_end_matches('…'))?;
    Ok(PubsubMessage {
        received_at: UNIX_EPOCH + Duration::from_secs(received_unix),
        kind,
        channel,
        payload,
    })
}

fn decode_hex(s: &str) -> Result<Vec<u8>, String> {
    if s.len() % 2 != 0 {
        return Err(format!("odd hex length: {}", s.len()));
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    for chunk in bytes.chunks_exact(2) {
        let hi = nybble(chunk[0])?;
        let lo = nybble(chunk[1])?;
        out.push((hi << 4) | lo);
    }
    Ok(out)
}

fn nybble(b: u8) -> Result<u8, String> {
    match b {
        b'0'..=b'9' => Ok(b - b'0'),
        b'a'..=b'f' => Ok(b - b'a' + 10),
        b'A'..=b'F' => Ok(b - b'A' + 10),
        _ => Err(format!("bad hex char: {:?}", b as char)),
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

    #[test]
    fn decode_hex_roundtrips_preview_output() {
        let bytes = vec![0x00, 0xde, 0xad, 0xbe, 0xef, 0x7f];
        let hex = hex_preview(&bytes, bytes.len() * 2);
        assert_eq!(decode_hex(&hex).unwrap(), bytes);
    }

    #[test]
    fn decode_hex_rejects_odd_length() {
        assert!(decode_hex("abc").is_err());
    }

    #[test]
    fn decode_hex_rejects_non_hex_char() {
        assert!(decode_hex("0g").is_err());
    }

    #[test]
    fn parse_history_line_round_trips_message() {
        let line = r#"{"received_unix":1730000000,"kind":"PSS","channel":"deadbeef","size":4,"payload_hex":"01020304"}"#;
        let m = parse_history_line(line).unwrap();
        assert_eq!(m.kind, PubsubKind::Pss);
        assert_eq!(m.channel, "deadbeef");
        assert_eq!(m.payload, vec![1, 2, 3, 4]);
        let secs = m.received_at.duration_since(UNIX_EPOCH).unwrap().as_secs();
        assert_eq!(secs, 1_730_000_000);
    }

    #[test]
    fn parse_history_line_handles_gsoc_kind() {
        let line = r#"{"received_unix":1,"kind":"GSOC","channel":"abc","size":0,"payload_hex":""}"#;
        let m = parse_history_line(line).unwrap();
        assert_eq!(m.kind, PubsubKind::Gsoc);
        assert!(m.payload.is_empty());
    }

    #[test]
    fn parse_history_line_rejects_unknown_kind() {
        let line = r#"{"received_unix":1,"kind":"WAT","channel":"x","size":0,"payload_hex":""}"#;
        assert!(parse_history_line(line).is_err());
    }

    #[test]
    fn rotation_path_appends_dot_n() {
        let p = rotation_path(Path::new("/tmp/pubsub.jsonl"), 3);
        assert_eq!(p, PathBuf::from("/tmp/pubsub.jsonl.3"));
    }

    #[tokio::test]
    async fn rotation_rolls_over_at_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        // Tiny rotate threshold so two short messages already trip it.
        let writer = open_history_writer(&path, 50, 3).await.unwrap();
        let now = SystemTime::now();
        for i in 0..6 {
            let m = PubsubMessage {
                received_at: now,
                kind: PubsubKind::Pss,
                channel: format!("ch{i}"),
                payload: vec![i as u8; 4],
            };
            append_history(&writer, &m).await;
        }
        // Active file exists.
        assert!(path.is_file(), "active file kept after rotation");
        // .1 exists (at minimum one rotation happened).
        assert!(path.with_extension("jsonl.1").is_file(), ".1 exists");
        // Never goes beyond .keep_files.
        assert!(
            !path.with_extension("jsonl.4").exists(),
            ".4 should not exist with keep_files=3"
        );
    }

    #[tokio::test]
    async fn replay_round_trips_messages_in_order() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        // No rotation — single file.
        let writer = open_history_writer(&path, 0, 0).await.unwrap();
        let now = SystemTime::now();
        let msgs = vec![
            PubsubMessage {
                received_at: now,
                kind: PubsubKind::Pss,
                channel: "topic-a".into(),
                payload: b"one".to_vec(),
            },
            PubsubMessage {
                received_at: now,
                kind: PubsubKind::Gsoc,
                channel: "abcdef".into(),
                payload: vec![0xff, 0xfe, 0xfd],
            },
        ];
        for m in &msgs {
            append_history(&writer, m).await;
        }
        // Drop the writer so the file flushes / closes before replay.
        drop(writer);
        let replayed = replay_history_file(&path).await.unwrap();
        assert_eq!(replayed.len(), 2);
        assert_eq!(replayed[0].channel, "topic-a");
        assert_eq!(replayed[0].payload, b"one");
        assert_eq!(replayed[1].kind, PubsubKind::Gsoc);
        assert_eq!(replayed[1].payload, vec![0xff, 0xfe, 0xfd]);
    }

    #[tokio::test]
    async fn replay_caps_at_max_messages() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let writer = open_history_writer(&path, 0, 0).await.unwrap();
        let now = SystemTime::now();
        for i in 0..(MAX_MESSAGES + 7) {
            let m = PubsubMessage {
                received_at: now,
                kind: PubsubKind::Pss,
                channel: "t".into(),
                payload: format!("p{i}").into_bytes(),
            };
            append_history(&writer, &m).await;
        }
        drop(writer);
        let replayed = replay_history_file(&path).await.unwrap();
        assert_eq!(replayed.len(), MAX_MESSAGES);
        // First retained line should be index 7 (we dropped 0..=6).
        assert_eq!(replayed[0].payload, b"p7");
    }

    #[tokio::test]
    async fn replay_skips_bad_lines() {
        use tokio::io::AsyncWriteExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("h.jsonl");
        let mut f = tokio::fs::File::create(&path).await.unwrap();
        f.write_all(b"{not json\n").await.unwrap();
        f.write_all(b"{\"received_unix\":1,\"kind\":\"PSS\",\"channel\":\"c\",\"size\":1,\"payload_hex\":\"ab\"}\n").await.unwrap();
        f.write_all(b"{\"missing\":\"fields\"}\n").await.unwrap();
        f.flush().await.unwrap();
        drop(f);
        let replayed = replay_history_file(&path).await.unwrap();
        assert_eq!(replayed.len(), 1);
        assert_eq!(replayed[0].payload, vec![0xab]);
    }
}
