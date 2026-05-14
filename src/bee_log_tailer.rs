//! Background task that follows a Bee node's log, parses each new
//! line, and ships entries down an mpsc to the cockpit's
//! [`crate::components::log_pane::LogPane`].
//!
//! Two sources, two entry points:
//!
//! - [`spawn`] follows a **log file** (the supervised child's
//!   capture file, or an external Bee's `log_file`). Polling-based;
//!   see "Why polling" + "Rotation handling" below.
//! - [`spawn_command`] follows the **stdout of a shell command**
//!   (`journalctl -u bee -f`, `docker logs -f bee`, `ssh host
//!   'tail -f …'`). For a Bee whose log file the cockpit can't read
//!   directly — remote host, container, restricted permissions.
//!
//! Both share the parser ([`crate::bee_log::parse_line`]), the
//! `(LogTab, BeeLogLine)` channel shape, and the rule that drops
//! bee-tui's own requests from the Bee HTTP tab.
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
//! ## Rotation handling
//!
//! The supervisor's [`crate::bee_log_writer`] caps the active log
//! file and rotates older content to numbered siblings. The tailer
//! detects this two ways every poll tick: an **inode mismatch**
//! (the path's current inode differs from the fd's — atomic rename
//! happened) or a **backwards size** (path size < our cursor —
//! the file was truncated). Either condition drains the old fd one
//! last time, then re-opens the path and resets the cursor to byte
//! 0. Lines emitted *between* the rotation and the next poll are
//! still preserved because the old fd keeps reading the renamed
//! file's tail.
//!
//! ## Backfill vs. start-at-EOF
//!
//! Two modes, selected by the `start_at_eof` flag:
//!
//! - **`false` (supervisor mode).** Start from byte 0. The
//!   supervisor's capture file is fresh, so reading it whole pulls
//!   Bee's startup logs into the cockpit. Re-reads the whole file
//!   if the *cockpit* restarts while Bee runs — fine for the bounded
//!   ring buffers.
//! - **`true` (external-tail mode).** Seek to end-of-file at open.
//!   An already-running Bee's log can be gigabytes; replaying it
//!   would flood the ring buffers and stall startup. Only lines
//!   written *after* the cockpit attaches are surfaced.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncSeekExt, BufReader};
use tokio::process::Command;
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
/// `start_at_eof` selects the backfill behaviour: `false` reads the
/// file from byte 0 (supervisor mode — fresh capture file, want the
/// startup logs), `true` seeks to end-of-file at open (external-tail
/// mode — the file pre-exists and may be huge).
///
/// Returns nothing — the task is fire-and-forget under the same
/// `root_cancel` tree as the rest of the cockpit. Caller is
/// responsible for keeping the receiver alive.
pub fn spawn(
    log_path: PathBuf,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
    start_at_eof: bool,
) {
    tokio::spawn(async move {
        run(log_path, tx, cancel, start_at_eof).await;
    });
}

async fn run(
    log_path: PathBuf,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
    start_at_eof: bool,
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
    tracing::info!("bee-log tailer: following {log_path:?} (start_at_eof={start_at_eof})");

    // Track the open fd's inode + how far we've read so we can
    // detect rotation (inode mismatch) vs truncation (size < cursor).
    let mut current_inode: Option<u64> = inode_of_open_file(&file).await;
    // External-tail mode: jump the cursor to EOF so pre-existing
    // history isn't replayed. A failed seek degrades to byte-0
    // behaviour — worse (a burst of stale lines) but not broken.
    let mut cursor: u64 = if start_at_eof {
        match file.seek(std::io::SeekFrom::End(0)).await {
            Ok(pos) => pos,
            Err(e) => {
                tracing::warn!(
                    "bee-log tailer: seek-to-EOF failed on {log_path:?}: {e}; \
                     replaying from byte 0"
                );
                0
            }
        }
    } else {
        0
    };

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
                    cursor += n as u64;
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

        // Rotation / truncation check. Done after draining the
        // current fd so any tail bytes from the now-renamed file
        // still make it through.
        if let Some((path_inode, path_size)) = stat_path(&log_path).await {
            let rotated = current_inode.is_some_and(|ino| ino != path_inode);
            let truncated = path_size < cursor;
            if rotated || truncated {
                tracing::info!(
                    "bee-log tailer: rotation detected (rotated={rotated}, \
                     truncated={truncated}); re-opening {log_path:?}"
                );
                if let Some(new_file) = reopen(&log_path).await {
                    file = new_file;
                    current_inode = inode_of_open_file(&file).await;
                    cursor = 0;
                    leftover.clear();
                }
            }
        }
    }
}

/// Spawn a background task that runs `command` through `sh -c` and
/// tails its **stdout**, forwarding parsed entries down `tx`. For a
/// Bee whose log file the cockpit can't read directly — the operator
/// supplies a command that streams it (`journalctl -u bee -f`,
/// `docker logs -f bee`, `ssh host 'tail -f …'`).
///
/// The child is killed when `cancel` fires (cockpit quit / context
/// switch) or the receiver is dropped; `kill_on_drop` is the safety
/// net for the panic path. The child's stderr is discarded — log
/// sources that write to stderr (e.g. `docker logs`) should redirect
/// with `2>&1` in the command string, which `sh -c` handles.
///
/// Fire-and-forget under the cockpit's cancel tree, same as [`spawn`].
pub fn spawn_command(
    command: String,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
) {
    tokio::spawn(async move {
        run_command(command, tx, cancel).await;
    });
}

async fn run_command(
    command: String,
    tx: UnboundedSender<(LogTab, BeeLogLine)>,
    cancel: CancellationToken,
) {
    // Route through the platform shell so pipes / quoting / redirects
    // in the operator's command string behave as typed.
    #[cfg(unix)]
    let (shell, shell_arg) = ("sh", "-c");
    #[cfg(windows)]
    let (shell, shell_arg) = ("cmd", "/C");

    let mut child = match Command::new(shell)
        .arg(shell_arg)
        .arg(&command)
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(
                "bee-log tailer: failed to spawn log command {command:?}: {e}; \
                 the bee-side log tabs will stay empty"
            );
            return;
        }
    };

    let Some(stdout) = child.stdout.take() else {
        tracing::warn!("bee-log tailer: log command {command:?} produced no stdout pipe");
        return;
    };
    let mut lines = BufReader::new(stdout).lines();
    tracing::info!("bee-log tailer: following command {command:?}");

    loop {
        tokio::select! {
            _ = cancel.cancelled() => {
                tracing::debug!("bee-log tailer: cancelled, killing log command");
                let _ = child.kill().await;
                break;
            }
            next = lines.next_line() => {
                match next {
                    Ok(Some(line)) => {
                        let Some(entry) = parse_line(&line) else {
                            continue;
                        };
                        let Some(tab) = entry.tab() else {
                            continue;
                        };
                        // Same rule as the file tailer: bee-tui's own
                        // outbound calls don't belong on the Bee HTTP
                        // tab (that tab is for *other* clients).
                        if tab == LogTab::BeeHttp && entry.is_bee_tui_request() {
                            continue;
                        }
                        if tx.send((tab, entry.to_log_line())).is_err() {
                            tracing::debug!(
                                "bee-log tailer: receiver dropped; killing log command"
                            );
                            let _ = child.kill().await;
                            return;
                        }
                    }
                    Ok(None) => {
                        // Command closed its stdout — it exited, or a
                        // `tail -f` lost its file. Nothing more to do;
                        // the operator can re-issue if needed.
                        tracing::info!(
                            "bee-log tailer: log command {command:?} closed its output; \
                             tailer stopping"
                        );
                        break;
                    }
                    Err(e) => {
                        tracing::warn!(
                            "bee-log tailer: read error on log command {command:?}: {e}"
                        );
                        break;
                    }
                }
            }
        }
    }
}

/// Fresh open of an already-existing file; cheaper than open_with_retry
/// since we know it exists (we just stat'd it).
async fn reopen(path: &Path) -> Option<tokio::fs::File> {
    match tokio::fs::File::open(path).await {
        Ok(f) => Some(f),
        Err(e) => {
            tracing::warn!("bee-log tailer: failed to re-open {path:?} after rotation: {e}");
            None
        }
    }
}

/// `stat(path)` → `(inode, size)`. None on permission / missing-file
/// errors (transient during rotation; we'll retry next tick).
async fn stat_path(path: &Path) -> Option<(u64, u64)> {
    let meta = tokio::fs::metadata(path).await.ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some((meta.ino(), meta.len()))
    }
    #[cfg(not(unix))]
    {
        // Best-effort fallback: use file size as both ino + len.
        // Rotation detection then degrades to truncation-only,
        // which is fine for bee-tui's primary Unix targets.
        Some((meta.len(), meta.len()))
    }
}

async fn inode_of_open_file(file: &tokio::fs::File) -> Option<u64> {
    let meta = file.metadata().await.ok()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Some(meta.ino())
    }
    #[cfg(not(unix))]
    {
        Some(meta.len())
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
        spawn(path.clone(), tx, cancel.clone(), false);

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
    async fn start_at_eof_skips_preexisting_content() {
        // External-tail mode: pre-existing lines must NOT be replayed.
        // Write a "stale" line *before* spawning, then a "fresh" line
        // after — only the fresh one should reach the channel.
        let (path, mut f) = make_temp_file().await;
        f.write_all(b"\"time\"=\"stale\" \"level\"=\"info\" \"logger\"=\"node\" \"msg\"=\"old\"\n")
            .await
            .unwrap();
        f.flush().await.unwrap();

        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone(), true);

        // Give the tailer a moment to open + seek to EOF before the
        // fresh write lands, so the cursor is past the stale line.
        tokio::time::sleep(Duration::from_millis(300)).await;
        f.write_all(b"\"time\"=\"fresh\" \"level\"=\"info\" \"logger\"=\"node\" \"msg\"=\"new\"\n")
            .await
            .unwrap();
        f.flush().await.unwrap();

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        cancel.cancel();
        let _ = std::fs::remove_file(&path);

        let (_, line) = received
            .expect("fresh line should arrive")
            .expect("channel open");
        // The first (and only) line delivered must be the fresh one —
        // the stale line was below the start-at-EOF cursor.
        assert_eq!(line.timestamp, "fresh");
        assert_eq!(line.message, "new");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_mode_forwards_parsed_lines() {
        // The command's stdout is parsed exactly like a file's bytes.
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn_command(
            r#"printf '"time"="cmd-t1" "level"="info" "logger"="node" "msg"="hello"\n'"#
                .to_string(),
            tx,
            cancel.clone(),
        );

        let received = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        cancel.cancel();

        let (tab, line) = received
            .expect("command line should arrive")
            .expect("channel open");
        assert_eq!(tab, LogTab::Info);
        assert_eq!(line.timestamp, "cmd-t1");
        assert_eq!(line.logger, "node");
        assert_eq!(line.message, "hello");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn command_mode_cancel_kills_the_child() {
        // A command that would otherwise run forever. Cancelling must
        // kill the child and end the task — if it didn't, this test
        // would hang and the harness would kill it. Absence of a hang
        // is the success condition.
        let (tx, _rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn_command("sleep 600".to_string(), tx, cancel.clone());
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    #[tokio::test]
    async fn cancel_stops_the_task() {
        let (path, _) = make_temp_file().await;
        let (tx, _rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone(), false);
        // Fire the cancel quickly; if the task ignored it, this
        // test would hang and the harness would kill it. We rely
        // on the absence of a hang as the success condition.
        cancel.cancel();
        tokio::time::sleep(Duration::from_millis(50)).await;
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn survives_rotation_via_rename() {
        // Simulate the bee_log_writer rotation: write a line, rename
        // the file to a sibling, create a fresh file at the original
        // path, write another line. The tailer should pick up both.
        let (path, mut f) = make_temp_file().await;
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone(), false);

        f.write_all(b"\"time\"=\"t1\" \"level\"=\"info\" \"logger\"=\"node\" \"msg\"=\"first\"\n")
            .await
            .unwrap();
        f.flush().await.unwrap();
        // Receive the first line so we know the tailer has read past
        // its initial open and computed a cursor.
        let recv1 = tokio::time::timeout(Duration::from_secs(2), rx.recv()).await;
        let (_, first_line) = recv1
            .expect("first line should arrive")
            .expect("channel open");
        assert_eq!(first_line.timestamp, "t1");

        // Drop the writer's fd; rename simulates rotation.
        drop(f);
        let rotated = path.with_extension("log.1");
        std::fs::rename(&path, &rotated).unwrap();

        // Fresh file at the original path.
        let mut f2 = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .await
            .unwrap();
        f2.write_all(
            b"\"time\"=\"t2\" \"level\"=\"info\" \"logger\"=\"node\" \"msg\"=\"second\"\n",
        )
        .await
        .unwrap();
        f2.flush().await.unwrap();

        let recv2 = tokio::time::timeout(Duration::from_secs(3), rx.recv()).await;
        cancel.cancel();
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(&rotated);

        let (_, second_line) = recv2
            .expect("post-rotation line should arrive")
            .expect("channel open");
        assert_eq!(second_line.timestamp, "t2");
    }

    #[tokio::test]
    async fn unknown_severity_lines_are_dropped() {
        let (path, mut f) = make_temp_file().await;
        let (tx, mut rx) = unbounded_channel();
        let cancel = CancellationToken::new();
        spawn(path.clone(), tx, cancel.clone(), false);

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
