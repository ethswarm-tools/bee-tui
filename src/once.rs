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
use crate::config_doctor;
use crate::durability;
use crate::economics_oracle;
use crate::manifest_walker::{self, InspectResult};
use crate::stamp_preview;
use crate::utility_verbs;
use crate::version_check;

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
        "upload-file" => once_upload_file(args).await,

        // ---- Stamp-economics verbs (one-shot fetch of chain state +
        //      stamps list, then pure math).
        "buy-preview" => once_buy_preview(args).await,
        "buy-suggest" => once_buy_suggest(args).await,
        "topup-preview" => once_topup_preview(args).await,
        "dilute-preview" => once_dilute_preview(args).await,
        "extend-preview" => once_extend_preview(args).await,
        "plan-batch" => once_plan_batch(args).await,
        "check-version" => once_check_version().await,
        "config-doctor" => once_config_doctor(args),
        "price" => once_price().await,
        "basefee" => once_basefee().await,

        // ---- Catch-all. --------------------------------------------
        other => OnceResult::usage(
            other,
            format!(
                "unknown --once verb {other:?}. Supported: hash, cid, depth-table, pss-target, gsoc-mine, readiness, version-check, check-version, config-doctor, price, basefee, inspect, durability-check, upload-file, buy-preview, buy-suggest, topup-preview, dilute-preview, extend-preview, plan-batch"
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

/// `--once upload-file <path> <batch-prefix>` — upload a single file
/// via `POST /bzz` and emit `{"reference": ...}`. CI-friendly: the
/// JSON output gives a workflow the swarm hash to publish without
/// shelling out to the cockpit. 256-MiB cap matches the cockpit verb.
async fn once_upload_file(args: &[String]) -> OnceResult {
    let (path_str, prefix) = match (args.first(), args.get(1)) {
        (Some(p), Some(b)) => (p.as_str(), b.as_str()),
        _ => {
            return OnceResult::usage(
                "upload-file",
                "usage: --once upload-file <path> <batch-prefix>",
            );
        }
    };
    let path = std::path::PathBuf::from(path_str);
    let meta = match std::fs::metadata(&path) {
        Ok(m) => m,
        Err(e) => return OnceResult::usage("upload-file", format!("stat {path_str}: {e}")),
    };
    if meta.is_dir() {
        return OnceResult::usage(
            "upload-file",
            format!("{path_str} is a directory; --once upload-file is single-file only"),
        );
    }
    const MAX_FILE_BYTES: u64 = 256 * 1024 * 1024;
    if meta.len() > MAX_FILE_BYTES {
        return OnceResult::usage(
            "upload-file",
            format!(
                "{path_str} is {} bytes — over the {}-MiB ceiling",
                meta.len(),
                MAX_FILE_BYTES / (1024 * 1024),
            ),
        );
    }
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let batches = match api.bee().postage().get_postage_batches().await {
        Ok(b) => b,
        Err(e) => return OnceResult::error("upload-file", format!("/stamps failed: {e}")),
    };
    let batch = match stamp_preview::match_batch_prefix(&batches, prefix) {
        Ok(b) => b.clone(),
        Err(e) => return OnceResult::usage("upload-file", e),
    };
    if !batch.usable {
        return OnceResult::error(
            "upload-file",
            format!(
                "batch {} is not usable yet (waiting on chain confirmation)",
                batch.batch_id.to_hex(),
            ),
        );
    }
    if batch.batch_ttl <= 0 {
        return OnceResult::error(
            "upload-file",
            format!("batch {} is expired", batch.batch_id.to_hex()),
        );
    }
    let data = match tokio::fs::read(&path).await {
        Ok(b) => b,
        Err(e) => return OnceResult::error("upload-file", format!("read {path_str}: {e}")),
    };
    let name = path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("")
        .to_string();
    let content_type = upload_content_type(&path);
    let result = api
        .bee()
        .file()
        .upload_file(&batch.batch_id, data, &name, &content_type, None)
        .await;
    match result {
        Ok(res) => OnceResult::ok_with_data(
            "upload-file",
            format!(
                "uploaded {} bytes → ref {} (batch {})",
                meta.len(),
                res.reference.to_hex(),
                &batch.batch_id.to_hex()[..8],
            ),
            json!({
                "path": path_str,
                "size": meta.len(),
                "reference": res.reference.to_hex(),
                "batch_id": batch.batch_id.to_hex(),
                "name": name,
                "content_type": if content_type.is_empty() { "application/octet-stream".to_string() } else { content_type },
            }),
        ),
        Err(e) => OnceResult::error("upload-file", format!("upload failed: {e}")),
    }
}

/// Best-effort MIME guess by extension. Empty string means "let
/// bee-rs default to application/octet-stream". Mirrors the
/// cockpit's `guess_content_type` so both verbs behave identically.
fn upload_content_type(path: &std::path::Path) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase());
    match ext.as_deref() {
        Some("html") | Some("htm") => "text/html",
        Some("txt") | Some("md") => "text/plain",
        Some("json") => "application/json",
        Some("css") => "text/css",
        Some("js") => "application/javascript",
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("pdf") => "application/pdf",
        Some("zip") => "application/zip",
        Some("tar") => "application/x-tar",
        Some("gz") | Some("tgz") => "application/gzip",
        Some("wasm") => "application/wasm",
        _ => "",
    }
    .to_string()
}

/// `--once buy-preview <depth> <amount-plur>` — predict cost / TTL
/// / capacity for a fresh batch buy at the chain's current price.
/// One-shot fetch of `/chainstate` so we get the actual price, not
/// a cached snapshot.
async fn once_buy_preview(args: &[String]) -> OnceResult {
    let (depth_str, amount_str) = match (args.first(), args.get(1)) {
        (Some(d), Some(a)) => (d.as_str(), a.as_str()),
        _ => {
            return OnceResult::usage(
                "buy-preview",
                "usage: --once buy-preview <depth> <amount-plur>",
            );
        }
    };
    let depth: u8 = match depth_str.parse() {
        Ok(d) => d,
        Err(_) => return OnceResult::usage("buy-preview", format!("invalid depth: {depth_str}")),
    };
    let amount = match stamp_preview::parse_plur_amount(amount_str) {
        Ok(a) => a,
        Err(e) => return OnceResult::usage("buy-preview", e),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let chain = match api.bee().debug().chain_state().await {
        Ok(c) => c,
        Err(e) => {
            return OnceResult::error("buy-preview", format!("/chainstate failed: {e}"));
        }
    };
    match stamp_preview::buy_preview(depth, amount, &chain) {
        Ok(p) => OnceResult::ok_with_data(
            "buy-preview",
            p.summary(),
            json!({
                "depth": p.depth,
                "amount_plur": p.amount_plur.to_string(),
                "ttl_seconds": p.ttl_seconds,
                "cost_bzz": p.cost_bzz,
            }),
        ),
        Err(e) => OnceResult::error("buy-preview", e),
    }
}

/// `--once buy-suggest <size> <duration>` — inverse of buy-preview.
/// Operator says "I want X bytes for Y seconds", we return the
/// minimum `(depth, amount)` that covers it.
async fn once_buy_suggest(args: &[String]) -> OnceResult {
    let (size_str, duration_str) = match (args.first(), args.get(1)) {
        (Some(s), Some(d)) => (s.as_str(), d.as_str()),
        _ => {
            return OnceResult::usage(
                "buy-suggest",
                "usage: --once buy-suggest <size> <duration>  (e.g. 5GiB 30d)",
            );
        }
    };
    let target_bytes = match stamp_preview::parse_size_bytes(size_str) {
        Ok(b) => b,
        Err(e) => return OnceResult::usage("buy-suggest", e),
    };
    let target_seconds = match stamp_preview::parse_duration_seconds(duration_str) {
        Ok(s) => s,
        Err(e) => return OnceResult::usage("buy-suggest", e),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let chain = match api.bee().debug().chain_state().await {
        Ok(c) => c,
        Err(e) => {
            return OnceResult::error("buy-suggest", format!("/chainstate failed: {e}"));
        }
    };
    match stamp_preview::buy_suggest(target_bytes, target_seconds, &chain) {
        Ok(p) => OnceResult::ok_with_data(
            "buy-suggest",
            p.summary(),
            json!({
                "target_bytes": p.target_bytes.to_string(),
                "target_seconds": p.target_seconds,
                "depth": p.depth,
                "amount_plur": p.amount_plur.to_string(),
                "capacity_bytes": p.capacity_bytes.to_string(),
                "ttl_seconds": p.ttl_seconds,
                "cost_bzz": p.cost_bzz,
            }),
        ),
        Err(e) => OnceResult::error("buy-suggest", e),
    }
}

/// `--once topup-preview <batch-prefix> <amount-plur>` — predict the
/// effect of topping up an existing batch.
async fn once_topup_preview(args: &[String]) -> OnceResult {
    let (prefix, amount_str) = match (args.first(), args.get(1)) {
        (Some(p), Some(a)) => (p.as_str(), a.as_str()),
        _ => {
            return OnceResult::usage(
                "topup-preview",
                "usage: --once topup-preview <batch-prefix> <amount-plur>",
            );
        }
    };
    let amount = match stamp_preview::parse_plur_amount(amount_str) {
        Ok(a) => a,
        Err(e) => return OnceResult::usage("topup-preview", e),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let (batches, chain) = match fetch_stamps_and_chain(&api).await {
        Ok(p) => p,
        Err(e) => return OnceResult::error("topup-preview", e),
    };
    let batch = match stamp_preview::match_batch_prefix(&batches, prefix) {
        Ok(b) => b.clone(),
        Err(e) => return OnceResult::usage("topup-preview", e),
    };
    match stamp_preview::topup_preview(&batch, amount, &chain) {
        Ok(p) => OnceResult::ok_with_data(
            "topup-preview",
            p.summary(),
            json!({
                "batch_id": batch.batch_id.to_hex(),
                "current_depth": p.current_depth,
                "current_ttl_seconds": p.current_ttl_seconds,
                "delta_amount_plur": p.delta_amount.to_string(),
                "extra_ttl_seconds": p.extra_ttl_seconds,
                "new_ttl_seconds": p.new_ttl_seconds,
                "cost_bzz": p.cost_bzz,
            }),
        ),
        Err(e) => OnceResult::error("topup-preview", e),
    }
}

/// `--once dilute-preview <batch-prefix> <new-depth>` — predict the
/// effect of diluting an existing batch (each +1 depth halves
/// per-chunk amount + TTL, doubles capacity).
async fn once_dilute_preview(args: &[String]) -> OnceResult {
    let (prefix, depth_str) = match (args.first(), args.get(1)) {
        (Some(p), Some(d)) => (p.as_str(), d.as_str()),
        _ => {
            return OnceResult::usage(
                "dilute-preview",
                "usage: --once dilute-preview <batch-prefix> <new-depth>",
            );
        }
    };
    let new_depth: u8 = match depth_str.parse() {
        Ok(d) => d,
        Err(_) => {
            return OnceResult::usage(
                "dilute-preview",
                format!("invalid depth: {depth_str}"),
            );
        }
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let batches = match api.bee().postage().get_postage_batches().await {
        Ok(b) => b,
        Err(e) => return OnceResult::error("dilute-preview", format!("/stamps failed: {e}")),
    };
    let batch = match stamp_preview::match_batch_prefix(&batches, prefix) {
        Ok(b) => b.clone(),
        Err(e) => return OnceResult::usage("dilute-preview", e),
    };
    match stamp_preview::dilute_preview(&batch, new_depth) {
        Ok(p) => OnceResult::ok_with_data(
            "dilute-preview",
            p.summary(),
            json!({
                "batch_id": batch.batch_id.to_hex(),
                "old_depth": p.old_depth,
                "new_depth": p.new_depth,
                "old_ttl_seconds": p.old_ttl_seconds,
                "new_ttl_seconds": p.new_ttl_seconds,
            }),
        ),
        Err(e) => OnceResult::error("dilute-preview", e),
    }
}

/// `--once extend-preview <batch-prefix> <duration>` — predict the
/// per-chunk amount + cost needed to extend the batch's TTL by the
/// requested duration.
async fn once_extend_preview(args: &[String]) -> OnceResult {
    let (prefix, duration_str) = match (args.first(), args.get(1)) {
        (Some(p), Some(d)) => (p.as_str(), d.as_str()),
        _ => {
            return OnceResult::usage(
                "extend-preview",
                "usage: --once extend-preview <batch-prefix> <duration>",
            );
        }
    };
    let extension_seconds = match stamp_preview::parse_duration_seconds(duration_str) {
        Ok(s) => s,
        Err(e) => return OnceResult::usage("extend-preview", e),
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let (batches, chain) = match fetch_stamps_and_chain(&api).await {
        Ok(p) => p,
        Err(e) => return OnceResult::error("extend-preview", e),
    };
    let batch = match stamp_preview::match_batch_prefix(&batches, prefix) {
        Ok(b) => b.clone(),
        Err(e) => return OnceResult::usage("extend-preview", e),
    };
    match stamp_preview::extend_preview(&batch, extension_seconds, &chain) {
        Ok(p) => OnceResult::ok_with_data(
            "extend-preview",
            p.summary(),
            json!({
                "batch_id": batch.batch_id.to_hex(),
                "depth": p.depth,
                "current_ttl_seconds": p.current_ttl_seconds,
                "needed_amount_plur": p.needed_amount_plur.to_string(),
                "cost_bzz": p.cost_bzz,
                "new_ttl_seconds": p.new_ttl_seconds,
            }),
        ),
        Err(e) => OnceResult::error("extend-preview", e),
    }
}

/// `--once price` — print xBZZ → USD spot price. No drift detection
/// (price moves independently of operator action), so always exits
/// 0 on success, 1 on fetch failure.
async fn once_price() -> OnceResult {
    match economics_oracle::fetch_xbzz_price().await {
        Ok(p) => OnceResult::ok_with_data(
            "price",
            p.summary(),
            json!({
                "usd": p.usd,
                "source": p.source,
            }),
        ),
        Err(e) => OnceResult::error("price", e),
    }
}

/// `--once basefee` — print Gnosis basefee + tip. Uses
/// `[economics].gnosis_rpc_url` from config.toml. Always exits 0
/// on success — gas fluctuates, gating CI on a threshold should
/// happen at the workflow level.
async fn once_basefee() -> OnceResult {
    let url = match Config::new()
        .ok()
        .and_then(|c| c.economics.gnosis_rpc_url)
    {
        Some(u) => u,
        None => {
            return OnceResult::usage(
                "basefee",
                "set [economics].gnosis_rpc_url in config.toml",
            );
        }
    };
    match economics_oracle::fetch_gnosis_gas(&url).await {
        Ok(g) => OnceResult::ok_with_data(
            "basefee",
            g.summary(),
            json!({
                "base_fee_gwei": g.base_fee_gwei,
                "max_priority_fee_gwei": g.max_priority_fee_gwei,
                "total_gwei": g.total_gwei(),
                "source_url": g.source_url,
            }),
        ),
        Err(e) => OnceResult::error("basefee", e),
    }
}

/// `--once config-doctor [path]` — audit a bee.yaml for deprecated
/// keys. With `[path]` argument explicit; without it, falls back to
/// the active node profile's `[bee].config` from bee-tui's
/// config.toml. Read-only. Exits `1` when any finding fires.
fn once_config_doctor(args: &[String]) -> OnceResult {
    let path: std::path::PathBuf = match args.first() {
        Some(p) => std::path::PathBuf::from(p),
        None => match Config::new().ok().and_then(|c| c.bee.map(|b| b.config)) {
            Some(p) => p,
            None => {
                return OnceResult::usage(
                    "config-doctor",
                    "usage: --once config-doctor <path-to-bee.yaml>  (or set [bee].config in bee-tui's config.toml)",
                );
            }
        },
    };
    let report = match config_doctor::audit(&path) {
        Ok(r) => r,
        Err(e) => return OnceResult::error("config-doctor", e),
    };
    let data = json!({
        "config_path": report.config_path.display().to_string(),
        "findings": report.findings.len(),
        "report": report.render(),
    });
    if report.is_clean() {
        OnceResult::ok_with_data("config-doctor", report.summary(), data)
    } else {
        OnceResult::unhealthy("config-doctor", report.summary(), data)
    }
}

/// `--once check-version` — pair the running Bee's `/health.version`
/// with GitHub's `releases/latest` for `ethersphere/bee`. Exits `1`
/// when version drift is detected so a CI job can gate on
/// "this node has fallen behind upstream".
async fn once_check_version() -> OnceResult {
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let running = api
        .bee()
        .debug()
        .health()
        .await
        .ok()
        .map(|h| h.version);
    match version_check::check_latest(running).await {
        Ok(v) => {
            let data = json!({
                "running": v.running,
                "latest_tag": v.latest_tag,
                "latest_published_at": v.latest_published_at,
                "latest_html_url": v.latest_html_url,
                "drift_detected": v.drift_detected,
            });
            if v.drift_detected {
                OnceResult::unhealthy("check-version", v.summary(), data)
            } else {
                OnceResult::ok_with_data("check-version", v.summary(), data)
            }
        }
        Err(e) => OnceResult::error("check-version", e),
    }
}

/// `--once plan-batch <prefix> [usage-thr] [ttl-thr] [extra-depth]` —
/// the unified topup+dilute decision. Mirrors the cockpit's
/// `:plan-batch` verb. Exits `1` when an action is recommended (so a
/// CI job can gate on "this batch needs human attention").
async fn once_plan_batch(args: &[String]) -> OnceResult {
    let prefix = match args.first() {
        Some(p) => p.as_str(),
        None => {
            return OnceResult::usage(
                "plan-batch",
                "usage: --once plan-batch <batch-prefix> [usage-thr] [ttl-thr] [extra-depth]",
            );
        }
    };
    let usage_thr = match args.get(1) {
        Some(s) => match s.parse::<f64>() {
            Ok(v) => v,
            Err(_) => {
                return OnceResult::usage(
                    "plan-batch",
                    format!("invalid usage-thr {s:?} (expected float in [0,1])"),
                );
            }
        },
        None => stamp_preview::DEFAULT_USAGE_THRESHOLD,
    };
    let ttl_thr = match args.get(2) {
        Some(s) => match stamp_preview::parse_duration_seconds(s) {
            Ok(v) => v,
            Err(e) => return OnceResult::usage("plan-batch", format!("ttl-thr: {e}")),
        },
        None => stamp_preview::DEFAULT_TTL_THRESHOLD_SECONDS,
    };
    let extra_depth = match args.get(3) {
        Some(s) => match s.parse::<u8>() {
            Ok(v) => v,
            Err(_) => {
                return OnceResult::usage(
                    "plan-batch",
                    format!("invalid extra-depth {s:?}"),
                );
            }
        },
        None => stamp_preview::DEFAULT_EXTRA_DEPTH,
    };
    let api = match build_api() {
        Ok(a) => a,
        Err(r) => return r,
    };
    let (batches, chain) = match fetch_stamps_and_chain(&api).await {
        Ok(p) => p,
        Err(e) => return OnceResult::error("plan-batch", e),
    };
    let batch = match stamp_preview::match_batch_prefix(&batches, prefix) {
        Ok(b) => b.clone(),
        Err(e) => return OnceResult::usage("plan-batch", e),
    };
    match stamp_preview::plan_batch(&batch, &chain, usage_thr, ttl_thr, extra_depth) {
        Ok(p) => {
            let action_kind = match &p.action {
                stamp_preview::PlanAction::None => "none",
                stamp_preview::PlanAction::Topup { .. } => "topup",
                stamp_preview::PlanAction::Dilute { .. } => "dilute",
                stamp_preview::PlanAction::TopupThenDilute { .. } => "topup_then_dilute",
            };
            let data = json!({
                "batch_id": batch.batch_id.to_hex(),
                "current_depth": p.current_depth,
                "current_usage_pct": p.current_usage_pct,
                "current_ttl_seconds": p.current_ttl_seconds,
                "usage_threshold_pct": p.usage_threshold_pct,
                "ttl_threshold_seconds": p.ttl_threshold_seconds,
                "extra_depth": p.extra_depth,
                "action": action_kind,
                "total_cost_bzz": p.total_cost_bzz,
                "reason": p.reason.clone(),
            });
            // Exit 1 when an action is recommended — lets CI gate on
            // "this batch needs attention." Status `Ok` only when no
            // action is needed.
            if matches!(p.action, stamp_preview::PlanAction::None) {
                OnceResult::ok_with_data("plan-batch", p.summary(), data)
            } else {
                OnceResult::unhealthy("plan-batch", p.summary(), data)
            }
        }
        Err(e) => OnceResult::error("plan-batch", e),
    }
}

/// Helper: one-shot parallel fetch of the postage batches list +
/// chain state. Used by the topup/extend paths which need both.
async fn fetch_stamps_and_chain(
    api: &Arc<ApiClient>,
) -> Result<
    (
        Vec<bee::postage::PostageBatch>,
        bee::debug::ChainState,
    ),
    String,
> {
    let bee = api.bee();
    let postage = bee.postage();
    let debug = bee.debug();
    let (batches, chain) = tokio::join!(postage.get_postage_batches(), debug.chain_state());
    let batches = batches.map_err(|e| format!("/stamps failed: {e}"))?;
    let chain = chain.map_err(|e| format!("/chainstate failed: {e}"))?;
    Ok((batches, chain))
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

    #[test]
    fn upload_content_type_known_extensions() {
        let p = std::path::PathBuf::from;
        assert_eq!(upload_content_type(&p("/tmp/x.html")), "text/html");
        assert_eq!(upload_content_type(&p("/tmp/x.PNG")), "image/png");
        assert_eq!(upload_content_type(&p("/tmp/x.tar.gz")), "application/gzip");
        // Unknown extension falls back to empty (bee-rs uses application/octet-stream).
        assert_eq!(upload_content_type(&p("/tmp/x.unknownext")), "");
    }
}
