//! Async pprof + trace bundling for `:diagnose --pprof[=N]`.
//!
//! Bee exposes Go's standard `/debug/pprof/profile` (CPU profile) and
//! `/debug/pprof/trace` (execution trace) endpoints when started with
//! `--debug-api-enable=true`. Each one blocks for `seconds=N` while
//! the runtime samples; both are independent so we run them in
//! parallel.
//!
//! The result is a directory next to the existing `.txt` diagnostic
//! file that the operator can `tar -czf` and ship. We deliberately do
//! NOT pull in a `tar` dependency — the directory is self-contained
//! and the operator's shell already knows how to bundle it.

use std::path::PathBuf;
use std::time::Duration;

use reqwest::StatusCode;

/// Result of a successful pprof+trace fetch. The directory contains
/// `profile.pprof` + `trace.pprof` (Go's pprof binary format —
/// readable by `go tool pprof` and `pprof` web).
#[derive(Debug, Clone)]
pub struct PprofBundle {
    pub dir: PathBuf,
    pub profile_bytes: u64,
    pub trace_bytes: u64,
    pub seconds: u32,
}

impl PprofBundle {
    /// Operator-friendly summary line.
    pub fn summary(&self) -> String {
        format!(
            "pprof bundle ready · {}s sampling · profile {} · trace {} → {}",
            self.seconds,
            human_bytes(self.profile_bytes),
            human_bytes(self.trace_bytes),
            self.dir.display(),
        )
    }
}

/// Fetch `/debug/pprof/profile?seconds=N` + `/debug/pprof/trace?seconds=N`
/// in parallel and write each to `dir/`. The returned bundle's
/// summary line is the canonical operator-facing message.
pub async fn fetch_and_write(
    base_url: &str,
    auth_token: Option<String>,
    seconds: u32,
    dir: PathBuf,
) -> Result<PprofBundle, String> {
    std::fs::create_dir_all(&dir).map_err(|e| format!("mkdir {}: {e}", dir.display()))?;
    // Generous timeout: pprof blocks for `seconds` then streams; add
    // 30s headroom for trailing read latency.
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(u64::from(seconds) + 30))
        .build()
        .map_err(|e| format!("client build: {e}"))?;

    let base = base_url.trim_end_matches('/').to_string();
    let profile_url = format!("{base}/debug/pprof/profile?seconds={seconds}");
    let trace_url = format!("{base}/debug/pprof/trace?seconds={seconds}");

    let (profile, trace) = tokio::join!(
        fetch_one(&client, &profile_url, auth_token.as_deref()),
        fetch_one(&client, &trace_url, auth_token.as_deref()),
    );

    let profile_bytes = profile.map_err(|e| format!("profile: {e}"))?;
    let trace_bytes = trace.map_err(|e| format!("trace: {e}"))?;

    let profile_path = dir.join("profile.pprof");
    let trace_path = dir.join("trace.pprof");
    std::fs::write(&profile_path, &profile_bytes).map_err(|e| format!("write profile: {e}"))?;
    std::fs::write(&trace_path, &trace_bytes).map_err(|e| format!("write trace: {e}"))?;

    Ok(PprofBundle {
        dir,
        profile_bytes: profile_bytes.len() as u64,
        trace_bytes: trace_bytes.len() as u64,
        seconds,
    })
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    auth: Option<&str>,
) -> Result<Vec<u8>, String> {
    let mut req = client.get(url);
    if let Some(token) = auth {
        req = req.bearer_auth(token);
    }
    let resp = req.send().await.map_err(|e| format!("GET {url}: {e}"))?;
    let status = resp.status();
    if status == StatusCode::NOT_FOUND {
        return Err("404 — Bee's debug API isn't enabled. Add `--debug-api-enable=true` to your Bee start args (or set `debug-api-enable: true` in bee.yaml) and restart.".to_string());
    }
    if !status.is_success() {
        return Err(format!("{url} returned HTTP {status}"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("read body of {url}: {e}"))?;
    Ok(bytes.to_vec())
}

fn human_bytes(n: u64) -> String {
    const KIB: u64 = 1 << 10;
    const MIB: u64 = 1 << 20;
    if n >= MIB {
        format!("{:.1} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.1} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn human_bytes_picks_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(2048), "2.0 KiB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.0 MiB");
    }

    #[test]
    fn summary_line_includes_dir_and_byte_counts() {
        let b = PprofBundle {
            dir: PathBuf::from("/tmp/bee-tui-diagnostic-1234"),
            profile_bytes: 12_345,
            trace_bytes: 6_789,
            seconds: 60,
        };
        let s = b.summary();
        assert!(s.contains("60s sampling"));
        assert!(s.contains("/tmp/bee-tui-diagnostic-1234"));
        assert!(s.contains("KiB"));
    }
}
