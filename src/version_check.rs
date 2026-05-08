//! `:check-version` — long-running Bee operators silently drift
//! behind upstream. This module hits GitHub's `releases/latest` for
//! `ethersphere/bee` and pairs the answer with the running Bee
//! version (read from `/health`) so operators see "running 2.7.2 /
//! latest 2.8.0 (released 2026-04-15)" at a glance.
//!
//! Read-only — informational only. No auto-update, no chain
//! interaction. Honours bee-tui's "inform, don't act" stance.
//!
//! GitHub's unauthenticated REST API allows ~60 requests/hour per
//! source IP; an interactive cockpit operator burns ≪ 1/hr so we
//! don't need a cache. CI users hitting `--once check-version` from
//! a CD pipeline should rate-limit at the workflow level.

use std::time::Duration;

use serde::Deserialize;

/// Outcome of `:check-version`. Drift is intentionally a string the
/// operator reads, not a semver subtraction — different version
/// schemes (release candidates, pre-releases, dirty builds) make a
/// `latest - running` integer ambiguous.
#[derive(Debug, Clone)]
pub struct VersionStatus {
    /// Version string from Bee's `/health`. Often includes a git-sha
    /// suffix (`2.7.2-bcaf69d-dirty`).
    pub running: Option<String>,
    /// `tag_name` from GitHub's `releases/latest` (e.g. `v2.8.0`).
    pub latest_tag: String,
    /// Human title (`v2.8.0`) and `published_at` (RFC 3339).
    pub latest_published_at: String,
    pub latest_html_url: String,
    /// True iff `running` parses as a SemVer Major.Minor.Patch and
    /// `latest_tag` parses likewise and the two differ. False when
    /// they appear identical OR when either is unparseable.
    pub drift_detected: bool,
}

impl VersionStatus {
    pub fn summary(&self) -> String {
        let running = self
            .running
            .clone()
            .unwrap_or_else(|| "(unknown)".to_string());
        let drift_marker = if self.drift_detected {
            " · DRIFT — upgrade available"
        } else {
            ""
        };
        format!(
            "running {} / latest {} ({} · {}){drift_marker}",
            running, self.latest_tag, self.latest_published_at, self.latest_html_url,
        )
    }
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
    published_at: String,
}

/// Fetch GitHub's `releases/latest` for `ethersphere/bee`. Pure async;
/// the caller passes the running Bee version (or `None`) so this
/// module stays Bee-API-free.
pub async fn check_latest(running_version: Option<String>) -> Result<VersionStatus, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        // GitHub rejects requests without a User-Agent. We send our
        // crate name + version verbatim — easy to grep for in
        // GitHub's logs if rate-limited.
        .user_agent(concat!("bee-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get("https://api.github.com/repos/ethersphere/bee/releases/latest")
        .header("Accept", "application/vnd.github+json")
        .send()
        .await
        .map_err(|e| format!("GET github releases: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "GitHub releases API returned HTTP {}",
            resp.status()
        ));
    }
    let release: GitHubRelease = resp
        .json()
        .await
        .map_err(|e| format!("decode github response: {e}"))?;
    let drift = match (&running_version, &release.tag_name) {
        (Some(r), tag) => version_drift_detected(r, tag),
        _ => false,
    };
    Ok(VersionStatus {
        running: running_version,
        latest_tag: release.tag_name,
        latest_published_at: release.published_at,
        latest_html_url: release.html_url,
        drift_detected: drift,
    })
}

/// Compare `running` (e.g. `2.7.2-bcaf69d-dirty`) with `latest_tag`
/// (e.g. `v2.8.0`). Returns true when both parse as `Major.Minor.Patch`
/// and the triples differ. Lenient — anything we can't parse maps to
/// false (no drift surfaced) so the operator doesn't get a false
/// alarm on an unusual build label.
fn version_drift_detected(running: &str, latest_tag: &str) -> bool {
    let r = parse_semver(running);
    let l = parse_semver(latest_tag);
    match (r, l) {
        (Some(a), Some(b)) => a != b,
        _ => false,
    }
}

fn parse_semver(s: &str) -> Option<(u32, u32, u32)> {
    let s = s.strip_prefix('v').unwrap_or(s);
    // First three dot-separated tokens, stopping at any non-digit
    // suffix (e.g. `-bcaf69d-dirty`, `-rc1`).
    let mut iter = s.split(['.', '-']);
    let major: u32 = iter.next()?.parse().ok()?;
    let minor: u32 = iter.next()?.parse().ok()?;
    let patch_raw = iter.next()?;
    // Patch may itself carry a non-digit suffix on weird build
    // strings; trim to the leading digits.
    let patch_digits: String = patch_raw
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect();
    let patch: u32 = patch_digits.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_semver_strips_v_prefix() {
        assert_eq!(parse_semver("v2.7.2"), Some((2, 7, 2)));
        assert_eq!(parse_semver("2.7.2"), Some((2, 7, 2)));
    }

    #[test]
    fn parse_semver_handles_dirty_suffix() {
        assert_eq!(parse_semver("2.7.2-bcaf69d-dirty"), Some((2, 7, 2)));
        assert_eq!(parse_semver("v2.7.2-rc1"), Some((2, 7, 2)));
    }

    #[test]
    fn parse_semver_returns_none_on_non_numeric() {
        assert_eq!(parse_semver("alpha"), None);
        assert_eq!(parse_semver("2"), None);
        assert_eq!(parse_semver(""), None);
    }

    #[test]
    fn drift_detected_when_versions_differ() {
        assert!(version_drift_detected("2.7.2", "v2.8.0"));
        assert!(version_drift_detected("v2.7.2", "v2.7.3"));
    }

    #[test]
    fn drift_not_detected_when_versions_match() {
        assert!(!version_drift_detected("2.7.2", "v2.7.2"));
        assert!(!version_drift_detected("2.7.2-bcaf69d-dirty", "v2.7.2"));
    }

    #[test]
    fn drift_not_detected_when_either_unparseable() {
        // Lenient — don't false-alarm on weird formats.
        assert!(!version_drift_detected("alpha", "v2.7.2"));
        assert!(!version_drift_detected("2.7.2", "weird-tag"));
    }

    #[test]
    fn summary_renders_drift_marker_when_set() {
        let s = VersionStatus {
            running: Some("2.7.2".into()),
            latest_tag: "v2.8.0".into(),
            latest_published_at: "2026-04-15T10:00:00Z".into(),
            latest_html_url: "https://github.com/ethersphere/bee/releases/tag/v2.8.0".into(),
            drift_detected: true,
        }
        .summary();
        assert!(s.contains("DRIFT"), "{s}");
        assert!(s.contains("v2.8.0"));
    }

    #[test]
    fn summary_omits_drift_marker_when_unset() {
        let s = VersionStatus {
            running: Some("2.7.2".into()),
            latest_tag: "v2.7.2".into(),
            latest_published_at: "2026-03-01T10:00:00Z".into(),
            latest_html_url: "https://github.com/ethersphere/bee/releases/tag/v2.7.2".into(),
            drift_detected: false,
        }
        .summary();
        assert!(!s.contains("DRIFT"), "{s}");
    }
}
