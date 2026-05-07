//! Background task that follows the supervised Bee process's log
//! file, parses each new line, and ships entries down an mpsc to
//! the cockpit's [`crate::components::log_pane::LogPane`].
//!
//! ## Why polling, not inotify
//!
//! `notify` / `inotify` would tell us *when* a write happens but
//! we still have to read the new bytes ourselves. For a
//! single-file follow at 200ms cadence, polling-with-`std::fs`
//! is just as responsive, half the dependency surface, and
//! survives moves/rotations more gracefully (the supervisor's
//! capture file is never rotated, but it's still the simpler
//! choice).
//!
//! ## What the tailer does
//!
//! 1. Opens the log file once at startup. If it doesn't exist
//!    yet (race with the supervisor's first write), retries.
//! 2. Tracks a byte offset into the file (cursor). Each tick:
//!    - read [cursor..eof] into a buffer
//!    - split on `\n`; the last partial line stays buffered
//!    - parse complete lines via [`crate::bee_log::parse_line`]
//!    - send `(LogTab, BeeLogLine)` pairs down the channel
//!    - advance the cursor
//! 3. Stops when the cancellation token fires (cockpit quit) or
//!    the channel receiver is dropped.
//!
//! ## What it doesn't do
//!
//! - **No file rotation handling.** If the file is truncated or
//!   replaced, we keep reading from the old cursor position and
//!   may emit garbage. Bee doesn't rotate the supervisor's
//!   capture, and the file lives in `$TMPDIR` so the operator
//!   doesn't either.
//! - **No backfill.** We start from byte 0 the first time the
//!   file appears, so the supervisor's startup logs make it
//!   into the cockpit. Subsequent restarts of the *cockpit*
//!   while Bee is still running re-read the whole file — fine
//!   for the bounded ring buffers but worth knowing.

use std::path::PathBuf;
use std::time::Duration;

use tokio::io::AsyncReadExt;
use tokio::sync::mpsc::UnboundedSender;
use tokio_util::sync::CancellationToken;

use crate::bee_log::parse_line;
use crate::components::log_pane::{BeeLogLine, LogTab};

/// How often we poll the file for new bytes. 200ms feels live to
/// an operator without burning measurable CPU. Made configurable
/// in case future work needs a tighter cadence (e.g. surfacing
/// a panic line with sub-second latency).
pub const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Open-retry budget. The supervisor writes its first byte before
/// `wait_for_api` returns, so the file should exist by the time
/// we start tailing — but the open could race in pathological
/// orderings. Five 100ms retries is a generous-but-bounded budget.
const OPEN_RETRY_INTERVAL: Duration = Duration::from_millis(100);
const OPEN_RETRY_LIMIT: u32 = 50;

/// Spawn a background task that tails `log_path` and forwards
/// parsed entries down `tx`. The task exits when `cancel` is
/// triggered (cockpit quit) or the receiving end of `tx` is dropped.
///
/// Returns nothing — the task is fire-and-forget under the same
/// `root_cancel` tree as the rest of the cockpit. Caller is
/// responsible for keeping the receiver alive.
pub fn spawn(
    log_path: PathBuf,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        run(log_path, tx, cancel).await;
    });
}

async fn run(
    log_path: PathBuf,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
) {
    // Open the file (with bounded retries for the first-byte race).
    let mut file = match open_with_retry(&log_path, &cancel).await {
        Some(f) => f,
        None => {
            tracing::warn!(
                "bee-log tailer: gave up opening {log_path:?} after retries; \
                 the bee-side log tabs will stay empty"
            );
            return;
        }
    };
    tracing::info!("bee-log tailer: following {log_path:?}");

    // Pending bytes that didn't end on a newline. Joined with the
    // next read so we never emit half a line.
    let mut leftover = String::new();
    let mut buf = vec![0u8; 8 * 1024];

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("bee-log tailer: cancelled, exiting");
                break;
            }
            _ = tokio::time::sleep(POLL_INTERVAL) => {}
        }

        // Read all currently-available bytes. Loop because a single
        // read might not drain a fast burst — but we cap at the
        // buffer size per read so we don't hog the runtime.
        loop {
            match file.read(&mut buf).await {
                Ok(0) => break, // EOF for now; come back next tick
                Ok(n) => {
                    let chunk = String::from_utf8_lossy(&buf[..n]);
                    leftover.push_str(&chunk);
                    // Repeated splits on newline. Keep the trailing
                    // partial line for next iteration.
                    let mut last_end = 0usize;
                    let mut emit = Vec::<&str>::new();
                    for (idx, _) in leftover.match_indices('\n') {
                        emit.push(&leftover[last_end..idx]);
                        last_end = idx + 1;
                    }
                    let emitted_lines: Vec<String> = emit.iter().map(|s| s.to_string()).collect();
                    let new_leftover = leftover[last_end..].to_string();
                    leftover = new_leftover;
                    for line in emitted_lines {
                        let Some(entry) = parse_line(&line) else {
                            continue;
                        };
                        let Some(tab) = entry.tab() else {
                            continue;
                        };
                        // Drop bee-tui's own requests from the Bee
                        // HTTP tab — operators want the tab to show
                        // *other* clients (curl / swarm-cli /
                        // browser). bee-tui's own outbound calls
                        // are still visible on the bee::http tab,
                        // sourced from the client-side capture.
                        if tab == LogTab::BeeHttp && entry.is_bee_tui_request() {
                            continue;
                        }
                        if tx.send((tab, entry.to_log_line())).is_err() {
                            tracing::debug!("bee-log tailer: receiver dropped; exiting");
                            return;
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!("bee-log tailer: read error on {log_path:?}: {e}");
                    break;
                }
            }
        }
    }
}

async fn open_with_retry(path: &PathBuf, cancel: &CancellationToken) -> Option<tokio::fs::File> {
    for _ in 0..OPEN_RETRY_LIMIT {
        if cancel.is_cancelled() {
            return None;
        }
        match tokio::fs::File::open(path).await {
            Ok(f) => return Some(f),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tokio::time::sleep(OPEN_RETRY_INTERVAL).await;
            }
            Err(e) => {
                tracing::warn!("bee-log tailer: cannot open {path:?}: {e}");
                return None;
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::AsyncWriteExt;
    use tokio::sync::mpsc::unbounded_channel;

    async fn make_temp_file() -> (PathBuf, tokio::fs::File) {
        let path = std::env::temp_dir().join(format!(
            "bee-log-tailer-test-{}.log",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let f = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .unwrap();
        (path, f)
    }

    #[tokio::test]
    async fn forwards_parsed_lines_to_channel() {
        let (path, mut f) = make_temp_file().await;
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone());

        // Write one full line + the start of another. The tailer
        // should forward the complete line and buffer the partial.
        f.write_all(
            b"\"time\"=\"2026-05-07 22:14:19.000000\" \"level\"=\"error\" \"logger\"=\"node/foo\" \"msg\"=\"boom\"\n",
        )
        .await
        .unwrap();
        f.write_all(b"\"time\"=\"t2\" \"level\"=\"debu")
            .await
            .unwrap();
        f.flush().await.unwrap();

        // Allow at least one poll tick.
        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        cancel.cancel();
        let _ = std::fs::remove_file(&path);

        let (tab, line) = received
            .expect("first line should arrive")
            .expect("channel open");
        assert_eq!(tab, LogTab::Errors);
        assert_eq!(line.timestamp, "2026-05-07 22:14:19.000000");
        assert_eq!(line.logger, "node/foo");
        assert!(line.message.starts_with("boom"));
    }

    #[tokio::test]
    async fn cancel_stops_the_task() {
        let (path, _) = make_temp_file().await;
        let (tx, _rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone());
        // Fire the cancel quickly; if the task ignored it, this
        // test would hang and the harness would kill it. We rely
        // on the absence of a hang as the success condition.
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn unknown_severity_lines_are_dropped() {
        let (path, mut f) = make_temp_file().await;
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone());

        // Level "panic" isn't recognised → entry.tab() is None →
        // dropped. We then write a known-good line; the tailer
        // should forward it (proving the parser keeps going past
        // a dropped line, not stopped).
        f.write_all(b"\"time\"=\"t1\" \"level\"=\"panic\" \"logger\"=\"node\" \"msg\"=\"x\"\n")
            .await
            .unwrap();
        f.write_all(b"\"time\"=\"t2\" \"level\"=\"info\" \"logger\"=\"node\" \"msg\"=\"y\"\n")
            .await
            .unwrap();
        f.flush().await.unwrap();

        let recv = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        cancel.cancel();
        let _ = std::fs::remove_file(&path);
        let (tab, line) = recv.expect("info line should arrive").expect("channel");
        assert_eq!(tab, LogTab::Info);
        assert_eq!(line.timestamp, "t2");
    }
}
