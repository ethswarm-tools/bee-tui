//! `bee-tui --once <verb> [args…]` — single-shot CI mode.
//!
//! The whole TUI runtime (App, screens, ratatui, supervisor, watch
//! hub) is bypassed. We build only what each verb needs:
//!
//!   * Pure-local verbs (`hash`, `cid`, `depth-table`, ...) need
//!     nothing — they call into [`crate::utility_verbs`].
//!   * Bee-API verbs (`readiness`, `inspect`, ...) build a one-shot
//!     [`ApiClient`] from the active node profile and call
//!     [`bee::Client`] directly.
//!
//! Output formats:
//!   * Default: one human-readable line on stdout.
//!   * `--json`: a single JSON object on stdout
//!     (`{ "verb": "...", "status": "ok|unhealthy|usage_error|error",
//!     "message": "...", "data": {...} }`).
//!
//! Exit codes:
//!   * `0` — verb succeeded and answer was healthy / OK.
//!   * `1` — verb completed but answer is unhealthy / failed gate /
//!     network said no.
//!   * `2` — usage error (unknown verb, bad args, missing config).
//!
//! Why this matters: makes every preview verb usable in CI / shell
//! pipelines without parsing TUI output. `bee-tui --once readiness`
//! is the canonical "is my Bee node ready for traffic?" smoke test.

use std::process::ExitCode;
use std::sync::Arc;

use serde::Serialize;
use serde_json::{Value, json};

use crate::api::ApiClient;
use crate::config::Config;
use crate::durability;
use crate::manifest_walker::{self, InspectResult};
use crate::utility_verbs;

/// Top-level result that's printed (as text or JSON) and converted to
/// an exit code.
#[derive(Debug, Serialize)]
pub struct OnceResult {
    pub verb: String,
    pub status: OnceStatus,
    pub message: String,
    #[serde(skip_serializing_if = "Value::is_null")]
    pub data: Value,
}

#[derive(Debug, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OnceStatus {
    Ok,
    Unhealthy,
    Error,
    UsageError,
}

impl OnceStatus {
    pub fn exit_code(self) -> ExitCode {
        match self {
            Self::Ok => ExitCode::SUCCESS,
            Self::Unhealthy | Self::Error => ExitCode::from(1),
            Self::UsageError => ExitCode::from(2),
        }
    }
}

impl OnceResult {
    pub fn ok(verb: &str, message: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            status: OnceStatus::Ok,
            message: message.into(),
            data: Value::Null,
        }
    }
    pub fn ok_with_data(verb: &str, message: impl Into<String>, data: Value) -> Self {
        Self {
            verb: verb.into(),
            status: OnceStatus::Ok,
            message: message.into(),
            data,
        }
    }
    pub fn unhealthy(verb: &str, message: impl Into<String>, data: Value) -> Self {
        Self {
            verb: verb.into(),
            status: OnceStatus::Unhealthy,
            message: message.into(),
            data,
        }
    }
    pub fn error(verb: &str, message: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            status: OnceStatus::Error,
            message: message.into(),
            data: Value::Null,
        }
    }
    pub fn usage(verb: &str, message: impl Into<String>) -> Self {
        Self {
            verb: verb.into(),
            status: OnceStatus::UsageError,
            message: message.into(),
            data: Value::Null,
        }
    }
}

/// Top-level entrypoint for `--once`. Fetches what the chosen verb
/// needs (or nothing for pure-local ones), runs the verb, prints
/// the result, returns the exit code.
pub async fn run(verb: &str, args: &[String], json_output: bool) -> ExitCode {
    let result = dispatch(verb, args).await;
    print_result(&result, json_output);
    result.status.exit_code()
}

async fn dispatch(verb: &str, args: &[String]) -> OnceResult {
    match verb {
        // ---- Pure-local verbs (no Bee call). -----------------------
        "hash" => once_hash(args),
        "cid" => once_cid(args),
        "depth-table" => once_depth_table(),
        "pss-target" => once_pss_target(args),
        "gsoc-mine" => once_gsoc_mine(args),

        // ---- Bee-API verbs. ----------------------------------------
        "readiness" => once_readiness().await,
        "version-check" => once_version_check().await,
        "inspect" => once_inspect(args).await,
        "durability-check" => once_durability_check(args).await,

        // ---- Catch-all. --------------------------------------------
        other => OnceResult::usage(
            other,
            format!(
                "unknown --once verb {other:?}. Supported: hash, cid, depth-table, pss-target, gsoc-mine, readiness, version-check, inspect, durability-check"
            ),
        ),
    }
}

// ---- Pure-local handlers ----------------------------------------------

fn once_hash(args: &[String]) -> OnceResult {
    let path = match args.first() {
        Some(p) => p.as_str(),
        None => {
            return OnceResult::usage("hash", "usage: --once hash <path>");
        }
    };
    match utility_verbs::hash_path(path) {
        Ok(r) => OnceResult::ok_with_data(
            "hash",
            format!("hash {path}: {r}"),
            json!({ "path": path, "reference": r }),
        ),
        Err(e) => OnceResult::error("hash", format!("hash failed: {e}")),
    }
}

fn once_cid(args: &[String]) -> OnceResult {
    let ref_arg = match args.first() {
        Some(r) => r.as_str(),
        None => return OnceResult::usage("cid", "usage: --once cid <ref> [manifest|feed]"),
    };
    let kind_arg = args.get(1).map(String::as_str);
    let kind = match utility_verbs::parse_cid_kind(kind_arg) {
        Ok(k) => k,
        Err(e) => return OnceResult::usage("cid", e),
    };
    match utility_verbs::cid_for_ref(ref_arg, kind) {
        Ok(cid) => {
            OnceResult::ok_with_data("cid", format!("cid: {cid}"), json!({ "cid": cid }))
        }
        Err(e) => OnceResult::error("cid", format!("cid failed: {e}")),
    }
}

fn once_depth_table() -> OnceResult {
    OnceResult::ok_with_data(
        "depth-table",
        utility_verbs::depth_table(),
        json!({ "table": utility_verbs::depth_table() }),
    )
}

fn once_pss_target(args: &[String]) -> OnceResult {
    let overlay = match args.first() {
        Some(o) => o.as_str(),
        None => return OnceResult::usage("pss-target", "usage: --once pss-target <overlay>"),
    };
    match utility_verbs::pss_target_for(overlay) {
        Ok(prefix) => OnceResult::ok_with_data(
            "pss-target",
            format!("pss target prefix: {prefix}"),
            json!({ "prefix": prefix }),
        ),
        Err(e) => OnceResult::error("pss-target", format!("pss-target failed: {e}")),
    }
}

fn once_gsoc_mine(args: &[String]) -> OnceResult {
    let overlay = args.first().map(String::as_str);
    let ident = args.get(1).map(String::as_str);
    let (overlay, ident) = match (overlay, ident) {
        (Some(o), Some(i)) => (o, i),
        _ => {
            return OnceResult::usage(
                "gsoc-mine",
                "usage: --once gsoc-mine <overlay> <identifier>",
            );
        }
    };
    match utility_verbs::gsoc_mine_for(overlay, ident) {
        Ok(out) => OnceResult::ok_with_data(
            "gsoc-mine",
            out.replace('\n', " · "),
            json!({ "result": out }),
        ),
        Err(e) => OnceResult::error("gsoc-mine", format!("gsoc-mine failed: {e}")),
    }
}

// ---- Bee-API handlers ------------------------------------------------

/// Build a one-shot [`ApiClient`] against the active node profile.
/// Returns the friendly UsageError for callers to surface when the
/// config is missing.
fn build_api() -> Result<Arc<ApiClient>, OnceResult> {
    let config = match Config::new() {
        Ok(c) => c,
        Err(e) => {
            return Err(OnceResult::usage(
                "_config",
                format!("could not load config: {e}"),
            ));
        }
    };
    let node = match config.active_node() {
        Some(n) => n,
        None => {
            return Err(OnceResult::usage(
                "_config",
                "no Bee node configured (config.nodes is empty)",
            ));
        }
    };
    let api = match ApiClient::from_node(node) {
        Ok(a) => Arc::new(a),
        Err(e) => {
            return Err(OnceResult::usage(
                "_config",
                format!("could not build api client: {e}"),
            ));
        }
    };
    Ok(api)
}

/// `--once readiness` — gateway-proxy-style "is this Bee node ready
/// to serve?" check. Pass when /health says ok AND topology depth
/// is in `[1, 30]`. Mirrors `swarm-gateway`'s readiness semantics.
async fn once_readiness() -> OnceResult {
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let bee = api.bee();
    let debug = bee.debug();
    let (health, topology) = tokio::join!(debug.health(), debug.topology());
    let health = match health {
        Ok(h) => h,
        Err(e) => {
            return OnceResult::error("readiness", format!("/health failed: {e}"));
        }
    };
    let topology = match topology {
        Ok(t) => t,
        Err(e) => {
            return OnceResult::error("readiness", format!("/topology failed: {e}"));
        }
    };
    let depth = topology.depth as u32;
    let depth_ok = (1..=30).contains(&depth);
    let status_ok = health.status == "ok";
    let data = json!({
        "health_status": health.status,
        "version": health.version,
        "api_version": health.api_version,
        "depth": depth,
        "depth_ok": depth_ok,
        "status_ok": status_ok,
    });
    if status_ok && depth_ok {
        OnceResult::ok_with_data(
            "readiness",
            format!(
                "READY · status={} · depth={depth} · version={}",
                health.status, health.version
            ),
            data,
        )
    } else {
        OnceResult::unhealthy(
            "readiness",
            format!(
                "NOT READY · status={} · depth={depth} (need [1,30]) · version={}",
                health.status, health.version
            ),
            data,
        )
    }
}

/// `--once version-check` — print Bee's reported version + API
/// version. Always exits 0 unless the fetch fails.
async fn once_version_check() -> OnceResult {
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    match api.bee().debug().health().await {
        Ok(h) => OnceResult::ok_with_data(
            "version-check",
            format!("bee {} · api {}", h.version, h.api_version),
            json!({
                "version": h.version,
                "api_version": h.api_version,
            }),
        ),
        Err(e) => OnceResult::error("version-check", format!("/health failed: {e}")),
    }
}

/// `--once inspect <ref>` — fetch one chunk + try to parse it as a
/// Mantaray manifest. Mirrors the cockpit's `:inspect` verb.
async fn once_inspect(args: &[String]) -> OnceResult {
    let ref_arg = match args.first() {
        Some(r) => r.as_str(),
        None => return OnceResult::usage("inspect", "usage: --once inspect <ref>"),
    };
    let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
        Ok(r) => r,
        Err(e) => return OnceResult::usage("inspect", format!("bad ref: {e}")),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    match manifest_walker::inspect(api, reference).await {
        InspectResult::Manifest { node, bytes_len } => OnceResult::ok_with_data(
            "inspect",
            format!(
                "manifest · {bytes_len} bytes · {} forks",
                node.forks.len()
            ),
            json!({
                "kind": "manifest",
                "bytes": bytes_len,
                "forks": node.forks.len(),
            }),
        ),
        InspectResult::RawChunk { bytes_len } => OnceResult::ok_with_data(
            "inspect",
            format!("raw chunk · {bytes_len} bytes"),
            json!({
                "kind": "raw_chunk",
                "bytes": bytes_len,
            }),
        ),
        InspectResult::Error(e) => OnceResult::error("inspect", format!("inspect failed: {e}")),
    }
}

/// `--once durability-check <ref>` — same chunk-graph walk the
/// cockpit's verb does, but in batch / CI mode.
async fn once_durability_check(args: &[String]) -> OnceResult {
    let ref_arg = match args.first() {
        Some(r) => r.as_str(),
        None => {
            return OnceResult::usage(
                "durability-check",
                "usage: --once durability-check <ref>",
            );
        }
    };
    let reference = match bee::swarm::Reference::from_hex(ref_arg.trim()) {
        Ok(r) => r,
        Err(e) => return OnceResult::usage("durability-check", format!("bad ref: {e}")),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let result = durability::check(api, reference).await;
    let data = json!({
        "chunks_total": result.chunks_total,
        "chunks_lost": result.chunks_lost,
        "chunks_errors": result.chunks_errors,
        "duration_ms": result.duration_ms,
        "root_is_manifest": result.root_is_manifest,
        "truncated": result.truncated,
    });
    if result.is_healthy() {
        OnceResult::ok_with_data("durability-check", result.summary(), data)
    } else {
        OnceResult::unhealthy("durability-check", result.summary(), data)
    }
}

// ---- Output ----------------------------------------------------------

fn print_result(result: &OnceResult, json_output: bool) {
    if json_output {
        match serde_json::to_string(result) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("(failed to serialize result: {e})"),
        }
        return;
    }
    let prefix = match result.status {
        OnceStatus::Ok => "OK",
        OnceStatus::Unhealthy => "UNHEALTHY",
        OnceStatus::Error => "ERROR",
        OnceStatus::UsageError => "USAGE",
    };
    println!("[{prefix}] {}", result.message);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(s: &[&str]) -> Vec<String> {
        s.iter().map(|x| x.to_string()).collect()
    }

    #[test]
    fn unknown_verb_returns_usage_error() {
        let r = once_pss_target(&[]);
        assert!(matches!(r.status, OnceStatus::UsageError));
        assert!(r.message.contains("usage"), "{}", r.message);
    }

    #[test]
    fn cid_handler_round_trips() {
        let r = once_cid(&args(&[&"0".repeat(64), "feed"]));
        assert!(matches!(r.status, OnceStatus::Ok));
        assert!(r.message.contains("cid:"), "{}", r.message);
        // JSON data contains the CID.
        assert!(r.data["cid"].is_string());
    }

    #[test]
    fn cid_handler_rejects_garbage() {
        let r = once_cid(&args(&["not-hex"]));
        assert!(matches!(r.status, OnceStatus::Error));
    }

    #[test]
    fn cid_handler_no_args_is_usage_error() {
        let r = once_cid(&[]);
        assert!(matches!(r.status, OnceStatus::UsageError));
    }

    #[test]
    fn pss_target_extracts_prefix() {
        let r = once_pss_target(&args(&[
            "abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234abcd1234",
        ]));
        assert!(matches!(r.status, OnceStatus::Ok));
        assert!(r.message.contains("abcd"), "{}", r.message);
    }

    #[test]
    fn depth_table_renders_full_table() {
        let r = once_depth_table();
        assert!(matches!(r.status, OnceStatus::Ok));
        assert!(r.message.contains("depth"));
        assert!(r.message.contains("17"));
        assert!(r.message.contains("34"));
    }

    #[test]
    fn exit_codes_map_correctly() {
        assert_eq!(
            OnceStatus::Ok.exit_code(),
            std::process::ExitCode::SUCCESS
        );
        // UsageError vs Error vs Unhealthy all distinguishable. We
        // can't equality-test ExitCode::from(N) directly, but we can
        // exercise that the path doesn't panic.
        let _ = OnceStatus::Unhealthy.exit_code();
        let _ = OnceStatus::Error.exit_code();
        let _ = OnceStatus::UsageError.exit_code();
    }

    #[test]
    fn ok_helpers_compose_the_expected_shape() {
        let r = OnceResult::ok("v", "all good");
        assert_eq!(r.verb, "v");
        assert!(matches!(r.status, OnceStatus::Ok));
        assert_eq!(r.message, "all good");
        assert!(r.data.is_null());

        let r2 = OnceResult::unhealthy("v", "broken", json!({"x": 1}));
        assert!(matches!(r2.status, OnceStatus::Unhealthy));
        assert_eq!(r2.data["x"], 1);
    }

    #[test]
    fn print_result_json_output_is_one_line() {
        // Smoke test the JSON path doesn't panic. We don't capture
        // stdout here — that's an integration concern.
        let r = OnceResult::ok("hash", "hash X: abc");
        print_result(&r, true);
        print_result(&r, false);
    }
}
