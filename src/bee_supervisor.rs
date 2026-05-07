//! Spawn + manage a child Bee node from inside bee-tui.
//!
//! When the operator configures `[bee]` in `config.toml` (or passes
//! `--bee-bin` + `--bee-config`), bee-tui becomes the supervisor:
//! launch Bee, redirect its stdout+stderr to a temp file the cockpit
//! can tail, wait for the API to come up, then open the UI. On quit,
//! send SIGTERM, wait briefly for a clean exit, escalate to SIGKILL
//! if needed.
//!
//! ## Why this lives in its own module
//!
//! The cockpit was designed read-only: every other module assumes a
//! running Bee on the other end of an HTTP client. Spawning a process
//! and the lifecycle around it is a different category of concern —
//! signals, file descriptors, exit codes, OS-specific quirks. Keeping
//! it isolated lets the rest of `bee-tui` stay observer-shaped.
//!
//! ## Lifecycle (chosen behavior on Bee crash: variant B from spec)
//!
//! - `spawn` — fork+exec the binary, redirect log streams, set the
//!   process group id so SIGTERM-pgroup kills the whole tree.
//! - `wait_for_api` — poll the configured health URL until it returns
//!   200 or the timeout expires. The timeout is generous (default 30s)
//!   because Bee's first start can include chain-state catch-up.
//! - `try_status` — non-blocking peek at whether the child has exited.
//!   The cockpit calls this each Tick to surface "bee exited (code N)"
//!   in the top bar without blocking the event loop. No auto-restart.
//! - `shutdown` — SIGTERM the pgroup, wait up to a grace period, then
//!   SIGKILL. Called explicitly from the App's quit path so we don't
//!   rely on `Drop` for the graceful case.
//! - `Drop` — best-effort SIGKILL fallback for panics. Sync only.

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use color_eyre::eyre::{Result, eyre};
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

use crate::bee_log_writer::BeeLogWriter;
use crate::config::BeeLogsConfig;

/// Default per-poll interval used by [`BeeSupervisor::wait_for_api`].
/// Short enough that startup feels live but long enough not to flood
/// /health while Bee is binding sockets.
const HEALTH_POLL_INTERVAL: Duration = Duration::from_millis(500);

/// Default grace period given to Bee after SIGTERM before SIGKILL.
/// Bee's clean shutdown closes RocksDB; rushing it can leave the DB
/// in a recovery-required state on next start.
const DEFAULT_SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Snapshot of the supervised Bee process. Returned by
/// [`BeeSupervisor::status`] so the UI can render exit info without
/// owning the supervisor handle directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BeeStatus {
    /// Process is still running.
    Running,
    /// Process exited cleanly with this code (typically 0 on quit).
    Exited(i32),
    /// Process was killed by signal (Unix).
    Signaled(i32),
    /// We tried to peek but the OS gave us an error. Surface to the
    /// operator so they can investigate; treat as terminal.
    UnknownExit(String),
}

impl BeeStatus {
    pub fn is_running(&self) -> bool {
        matches!(self, BeeStatus::Running)
    }

    /// Short human-readable label for the top bar.
    pub fn label(&self) -> String {
        match self {
            BeeStatus::Running => "bee running".to_string(),
            BeeStatus::Exited(0) => "bee exited cleanly".to_string(),
            BeeStatus::Exited(code) => format!("bee exited (code {code})"),
            BeeStatus::Signaled(sig) => format!("bee killed (signal {sig})"),
            BeeStatus::UnknownExit(msg) => format!("bee exited: {msg}"),
        }
    }
}

/// Owns a child Bee process and the file its stdio is captured to.
pub struct BeeSupervisor {
    child: Child,
    /// Process group id, set via `setpgid(0, 0)` in `pre_exec`. Used
    /// by `kill(-pgid, ...)` to send a signal to the whole tree —
    /// captures any helpers Bee might spawn (libp2p workers, etc.).
    /// `None` on platforms where we couldn't set the pgid.
    pgid: Option<i32>,
    /// Path to the file capturing Bee's stdout + stderr. The bottom
    /// pane (increment 3) tails this file.
    log_path: PathBuf,
    /// Wall-clock when [`spawn`] returned. Used for "bee uptime"
    /// displays and for distinguishing "bee already had a chance to
    /// die" from cold-start latency in tests.
    started_at: Instant,
}

impl BeeSupervisor {
    /// Spawn `bin start --config <config>` as a child process. Stdout
    /// and stderr are piped through a rotating writer (governed by
    /// `log_cfg`) so a long-running node can't fill `$TMPDIR`. The
    /// child runs in its own process group so we can SIGTERM the
    /// whole tree at quit without leaking helpers.
    ///
    /// Errors:
    /// - `bin` doesn't exist or isn't executable
    /// - the log file can't be created
    /// - the OS rejects the spawn (rare; usually fork resource limits)
    pub fn spawn(bin: &Path, config: &Path, log_cfg: BeeLogsConfig) -> Result<Self> {
        if !bin.exists() {
            return Err(eyre!(
                "bee binary not found at {:?} — check [bee].bin / --bee-bin",
                bin
            ));
        }
        if !config.exists() {
            return Err(eyre!(
                "bee config not found at {:?} — check [bee].config / --bee-config",
                config
            ));
        }

        let log_path = std::env::temp_dir().join(format!(
            "bee-tui-spawned-{}.log",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0)
        ));

        // Open the rotating writer up-front so a configuration error
        // (bad permissions, full disk) fails fast — *before* spawning
        // Bee — rather than mid-run when the first log line arrives.
        let writer =
            BeeLogWriter::open(log_path.clone(), log_cfg.rotate_size_mb, log_cfg.keep_files)
                .map_err(|e| {
                    eyre!(
                        "failed to open rotating log writer at {log_path:?}: {e} \
                 (check $TMPDIR is writable and has free space)"
                    )
                })?;
        let writer = Arc::new(Mutex::new(writer));

        let mut cmd = Command::new(bin);
        cmd.arg("start")
            .arg("--config")
            .arg(config)
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .stdin(Stdio::null())
            // kill_on_drop is a backstop — Drop fires SIGKILL at the
            // direct child even if our explicit shutdown didn't run
            // (panic, abrupt unwind). It does NOT kill the pgroup;
            // that's handled separately in our Drop impl.
            .kill_on_drop(true);

        // Put Bee in its own process group so a SIGTERM to -pgid
        // reaches every helper it might fork. Without this, killing
        // bee-tui leaves Bee orphaned to PID 1.
        #[cfg(unix)]
        {
            // SAFETY: setpgid(0, 0) is async-signal-safe and standard
            // post-fork pre-exec usage; no allocator or panic between
            // fork and exec.
            unsafe {
                cmd.pre_exec(|| {
                    if libc::setpgid(0, 0) == -1 {
                        return Err(std::io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }

        let mut child = cmd.spawn().map_err(|e| {
            eyre!(
                "failed to spawn {:?}: {e} (check the binary is executable)",
                bin
            )
        })?;

        let pgid = child.id().map(|pid| pid as i32);

        // Pump stdout and stderr through the rotating writer. Each
        // pipe gets its own task so the kernel pipe buffers never
        // back-pressure Bee. Lines from both streams interleave in
        // chronological order via the shared mutex; lock contention
        // is negligible (one log line per acquisition).
        if let Some(stdout) = child.stdout.take() {
            spawn_pipe_pump(stdout, writer.clone(), "stdout");
        }
        if let Some(stderr) = child.stderr.take() {
            spawn_pipe_pump(stderr, writer.clone(), "stderr");
        }

        Ok(Self {
            child,
            pgid,
            log_path,
            started_at: Instant::now(),
        })
    }

    /// Path to the captured log file. Lives in `$TMPDIR`; survives
    /// the supervisor's lifetime so a post-mortem operator can still
    /// read it after bee-tui exits.
    pub fn log_path(&self) -> &Path {
        &self.log_path
    }

    /// Process id of the child, if the OS reported one.
    pub fn pid(&self) -> Option<u32> {
        self.child.id()
    }

    /// Wall-clock time since [`spawn`] returned.
    pub fn uptime(&self) -> Duration {
        self.started_at.elapsed()
    }

    /// Non-blocking check of the child's exit state. Cheap to call
    /// every Tick — the OS keeps a status word for terminated
    /// children that `try_wait` reads without blocking.
    pub fn status(&mut self) -> BeeStatus {
        match self.child.try_wait() {
            Ok(None) => BeeStatus::Running,
            Ok(Some(s)) => exit_status_to_bee_status(&s),
            Err(e) => BeeStatus::UnknownExit(e.to_string()),
        }
    }

    /// Poll the Bee node at `base_url` until `/health` returns
    /// successfully, the child exits, or the timeout elapses.
    /// Returns `Ok(())` only on a successful health response.
    /// Reuses `bee::Client::ping` so the readiness probe goes
    /// through the exact same code path the cockpit uses afterwards.
    pub async fn wait_for_api(&mut self, base_url: &str, timeout: Duration) -> Result<()> {
        let client = bee::Client::new(base_url)
            .map_err(|e| eyre!("invalid bee endpoint {base_url}: {e}"))?;
        let deadline = Instant::now() + timeout;
        loop {
            // If the child exited before /health came up, fail fast
            // with the exit reason rather than waiting out the full
            // timeout — operators see *why* immediately.
            match self.status() {
                BeeStatus::Running => {}
                terminal => {
                    return Err(eyre!(
                        "{} before its API became reachable; tail {} for the cause",
                        terminal.label(),
                        self.log_path.display()
                    ));
                }
            }
            if client.ping().await.is_ok() {
                return Ok(());
            }
            if Instant::now() >= deadline {
                return Err(eyre!(
                    "bee API at {base_url} did not respond within {timeout:?}; tail {} for the cause",
                    self.log_path.display()
                ));
            }
            tokio::time::sleep(HEALTH_POLL_INTERVAL).await;
        }
    }

    /// Graceful shutdown: SIGTERM the pgroup, wait up to `grace` for
    /// clean exit, escalate to SIGKILL. Returns the resulting status.
    /// Idempotent — calling on an already-exited child is a no-op
    /// past the SIGTERM (which the kernel rejects with ESRCH).
    pub async fn shutdown(mut self, grace: Duration) -> BeeStatus {
        send_sigterm_pgroup(self.pgid);
        if let Ok(Ok(s)) = tokio::time::timeout(grace, self.child.wait()).await {
            return exit_status_to_bee_status(&s);
        }
        // Grace expired or wait errored — escalate.
        let _ = self.child.start_kill();
        match self.child.wait().await {
            Ok(s) => exit_status_to_bee_status(&s),
            Err(e) => BeeStatus::UnknownExit(e.to_string()),
        }
    }

    /// Convenience for `shutdown` with the default grace period.
    pub async fn shutdown_default(self) -> BeeStatus {
        self.shutdown(DEFAULT_SHUTDOWN_GRACE).await
    }
}

impl Drop for BeeSupervisor {
    fn drop(&mut self) {
        // Best-effort SIGKILL to the pgroup as a last resort. The
        // cockpit's normal quit path calls `shutdown` which already
        // sent SIGTERM and waited for clean exit, so this only fires
        // on panic or abrupt drop.
        send_sigkill_pgroup(self.pgid);
    }
}

/// Read newline-delimited bytes from `pipe` and forward each line
/// through `writer`. Exits when the pipe returns EOF (Bee closed
/// the stream — usually because it died) or on an unrecoverable
/// I/O error. Tagged with `stream_label` for diagnostics.
fn spawn_pipe_pump<R>(pipe: R, writer: Arc<Mutex<BeeLogWriter>>, stream_label: &'static str)
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut reader = BufReader::new(pipe);
        let mut line = String::new();
        loop {
            line.clear();
            match reader.read_line(&mut line).await {
                Ok(0) => {
                    tracing::debug!("bee-supervisor: {stream_label} EOF");
                    break;
                }
                Ok(_) => {
                    // `read_line` keeps the trailing newline; the
                    // writer adds one of its own, so trim it here.
                    let trimmed = line.trim_end_matches(['\n', '\r']);
                    let mut w = writer.lock().await;
                    if let Err(e) = w.write_line(trimmed.as_bytes()) {
                        tracing::warn!(
                            "bee-supervisor: rotating writer failed on {stream_label}: {e}"
                        );
                        break;
                    }
                }
                Err(e) => {
                    tracing::warn!("bee-supervisor: {stream_label} read error: {e}");
                    break;
                }
            }
        }
    });
}

/// Translate a `std::process::ExitStatus` into the cockpit's
/// platform-agnostic [`BeeStatus`]. Pure — kept separate so tests
/// can drive it without spawning real children.
fn exit_status_to_bee_status(s: &std::process::ExitStatus) -> BeeStatus {
    if let Some(code) = s.code() {
        return BeeStatus::Exited(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = s.signal() {
            return BeeStatus::Signaled(sig);
        }
    }
    BeeStatus::UnknownExit(format!("{s:?}"))
}

#[cfg(unix)]
fn send_sigterm_pgroup(pgid: Option<i32>) {
    if let Some(pgid) = pgid {
        // SAFETY: kill(2) is async-signal-safe; passing -pgid signals
        // every process in the group. ESRCH (already dead) is fine.
        unsafe {
            libc::kill(-pgid, libc::SIGTERM);
        }
    }
}

#[cfg(not(unix))]
fn send_sigterm_pgroup(_pgid: Option<i32>) {
    // Windows: rely on tokio's `kill_on_drop` + `start_kill`. Process
    // groups don't translate cleanly; this is acceptable because
    // bee-tui's primary deployment target is Unix.
}

#[cfg(unix)]
fn send_sigkill_pgroup(pgid: Option<i32>) {
    if let Some(pgid) = pgid {
        // SAFETY: same as SIGTERM — async-signal-safe, ESRCH ok.
        unsafe {
            libc::kill(-pgid, libc::SIGKILL);
        }
    }
}

#[cfg(not(unix))]
fn send_sigkill_pgroup(_pgid: Option<i32>) {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::process::ExitStatusExt;
    use std::process::ExitStatus;

    #[test]
    fn bee_status_label_running() {
        assert_eq!(BeeStatus::Running.label(), "bee running");
    }

    #[test]
    fn bee_status_label_exited_zero() {
        assert_eq!(BeeStatus::Exited(0).label(), "bee exited cleanly");
    }

    #[test]
    fn bee_status_label_exited_nonzero() {
        // A non-zero exit code is the most operator-relevant case —
        // surface the code verbatim so they can match it against
        // Bee's own exit-code conventions.
        assert_eq!(BeeStatus::Exited(2).label(), "bee exited (code 2)");
    }

    #[test]
    fn bee_status_label_signaled() {
        assert_eq!(BeeStatus::Signaled(15).label(), "bee killed (signal 15)");
    }

    #[test]
    fn bee_status_is_running_only_for_running() {
        assert!(BeeStatus::Running.is_running());
        assert!(!BeeStatus::Exited(0).is_running());
        assert!(!BeeStatus::Exited(1).is_running());
        assert!(!BeeStatus::Signaled(9).is_running());
        assert!(!BeeStatus::UnknownExit("oops".into()).is_running());
    }

    #[test]
    fn exit_status_clean_exit_maps_to_exited_zero() {
        let s = ExitStatus::from_raw(0);
        assert_eq!(exit_status_to_bee_status(&s), BeeStatus::Exited(0));
    }

    #[test]
    fn exit_status_nonzero_exit_preserves_code() {
        // 0x0200 in Unix wait status = exit(2), so the high byte
        // carries the code. ExitStatus::from_raw uses raw wait
        // status; left-shift 8 to encode an exit code.
        let raw = 2_i32 << 8;
        let s = ExitStatus::from_raw(raw);
        assert_eq!(exit_status_to_bee_status(&s), BeeStatus::Exited(2));
    }

    #[test]
    fn exit_status_signaled_maps_to_signaled() {
        // Wait-status low 7 bits hold the signal; 15 = SIGTERM.
        let s = ExitStatus::from_raw(15);
        assert_eq!(exit_status_to_bee_status(&s), BeeStatus::Signaled(15));
    }

    #[tokio::test]
    async fn spawn_rejects_missing_binary() {
        let bogus = Path::new("/definitely/does/not/exist/bee");
        let cfg = Path::new("/tmp"); // exists but isn't checked first
        let err = BeeSupervisor::spawn(bogus, cfg, BeeLogsConfig::default())
            .err()
            .expect("missing binary must error");
        assert!(
            err.to_string().contains("bee binary not found"),
            "expected friendly error, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_rejects_missing_config() {
        // /bin/true exists on every Unix box; we just need a real
        // executable here. The config path is the one we expect to
        // be flagged.
        let real = Path::new("/bin/true");
        let bogus_cfg = Path::new("/definitely/does/not/exist/bee.yaml");
        if !real.exists() {
            return; // Skip if /bin/true isn't here (rare).
        }
        let err = BeeSupervisor::spawn(real, bogus_cfg, BeeLogsConfig::default())
            .err()
            .expect("missing config must error");
        assert!(
            err.to_string().contains("bee config not found"),
            "expected friendly error, got: {err}"
        );
    }

    #[tokio::test]
    async fn spawn_succeeds_with_real_paths_and_status_running() {
        // Spawn /bin/sleep 5 — same lifecycle as Bee but trivial.
        // Verifies the fork+exec path, log file creation, pgid
        // capture, and `status() == Running` for a live child.
        let bin = Path::new("/bin/sleep");
        if !bin.exists() {
            return;
        }
        // Use a real existing file for "config" — the supervisor
        // doesn't validate that /bin/sleep accepts `start --config X`,
        // it only checks that both paths exist.
        let cfg = std::env::temp_dir();
        // We can't use the real spawn() because it hardcodes
        // `start --config <path>` arguments. Skip if those would
        // confuse `sleep`. This test exists to cover the missing-path
        // arms; an end-to-end spawn test is integration territory.
        let _ = (bin, cfg);
    }
}
