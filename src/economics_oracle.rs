//! External cost-context oracles: xBZZ → USD price and Gnosis-chain
//! basefee + tip. Both are read-only fetches off services Bee
//! itself doesn't expose. Live behind opt-in commands so a flaky
//! external service can never block the cockpit.
//!
//! ## Why this module
//!
//! Operators do every economics decision (buy / topup / dilute /
//! cashout) in xBZZ but think in dollars and gwei. The cockpit
//! traditionally surfaces only the on-chain numbers; this module
//! brings the two missing dimensions in.
//!
//! ## Sources
//!
//! * **xBZZ → USD**: Swarm Foundation's
//!   `https://tokenservice.ethswarm.org/token_price` (the same
//!   endpoint swarm-desktop's `/price` proxies). Returns a JSON
//!   object whose `usd` field is the spot price.
//! * **Gnosis basefee + tip**: JSON-RPC against the operator's
//!   configured `[economics].gnosis_rpc_url`. We send
//!   `eth_getBlockByNumber("pending", false)` for the basefee and
//!   `eth_maxPriorityFeePerGas` for the tip in parallel.
//!
//! ## Caching
//!
//! Intentionally absent in v1. The verbs are operator-driven (or
//! one-shot in CI), so the call rate is bounded by user typing or
//! workflow runs — well under any rate limit. If the watch hub
//! picks these up later (auto-poll the price every 5 min, gas every
//! 30 s), we'll add a TTL cache here.

use std::time::{Duration, SystemTime};

use serde::Deserialize;

/// xBZZ spot price + provenance. `source` carries the URL the
/// price came from so operators can reason about the supply chain.
#[derive(Debug, Clone)]
pub struct XbzzPrice {
    pub usd: f64,
    pub fetched_at: SystemTime,
    pub source: String,
}

impl XbzzPrice {
    pub fn summary(&self) -> String {
        format!("xBZZ ≈ ${:.4} (source: {})", self.usd, self.source)
    }
}

/// Snapshot of Gnosis-chain gas pricing at the time of the call.
/// Both fields in gwei; tip is `None` when the RPC doesn't surface
/// `eth_maxPriorityFeePerGas` (older Erigon builds skip it).
#[derive(Debug, Clone)]
pub struct GasInfo {
    pub base_fee_gwei: f64,
    pub max_priority_fee_gwei: Option<f64>,
    pub fetched_at: SystemTime,
    pub source_url: String,
}

impl GasInfo {
    /// Total gas a transaction would pay at this snapshot, in gwei.
    pub fn total_gwei(&self) -> f64 {
        self.base_fee_gwei + self.max_priority_fee_gwei.unwrap_or(0.0)
    }
    pub fn summary(&self) -> String {
        match self.max_priority_fee_gwei {
            Some(tip) => format!(
                "gas: {:.2} base + {:.2} tip = {:.2} gwei (source: {})",
                self.base_fee_gwei,
                tip,
                self.total_gwei(),
                self.source_url,
            ),
            None => format!(
                "gas: {:.2} gwei base (tip unavailable; source: {})",
                self.base_fee_gwei, self.source_url,
            ),
        }
    }
}

const TOKEN_SERVICE_URL: &str = "https://tokenservice.ethswarm.org/token_price";

#[derive(Deserialize)]
struct TokenServicePayload {
    /// Older versions of the endpoint return `usd`; newer use `bzz`
    /// or `swarm`. Try `usd` first; fall back to a flat scan of any
    /// numeric value the response carries.
    #[serde(default)]
    usd: Option<f64>,
    #[serde(default, flatten)]
    other: serde_json::Value,
}

/// Fetch the current xBZZ → USD price.
pub async fn fetch_xbzz_price() -> Result<XbzzPrice, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("bee-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let resp = client
        .get(TOKEN_SERVICE_URL)
        .send()
        .await
        .map_err(|e| format!("GET {TOKEN_SERVICE_URL}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!(
            "tokenservice returned HTTP {}",
            resp.status()
        ));
    }
    let body: TokenServicePayload = resp
        .json()
        .await
        .map_err(|e| format!("decode tokenservice response: {e}"))?;
    let usd = body
        .usd
        .or_else(|| extract_first_numeric(&body.other))
        .ok_or_else(|| "tokenservice response had no numeric price field".to_string())?;
    if !usd.is_finite() || usd <= 0.0 {
        return Err(format!("tokenservice returned bogus price {usd}"));
    }
    Ok(XbzzPrice {
        usd,
        fetched_at: SystemTime::now(),
        source: TOKEN_SERVICE_URL.to_string(),
    })
}

/// Walk a flattened JSON value picking the first numeric leaf. Used
/// to keep us robust against the tokenservice payload changing
/// shape (e.g. `usd` → `bzz_usd_price`).
fn extract_first_numeric(v: &serde_json::Value) -> Option<f64> {
    match v {
        serde_json::Value::Number(n) => n.as_f64(),
        serde_json::Value::Object(map) => {
            for v in map.values() {
                if let Some(n) = extract_first_numeric(v) {
                    return Some(n);
                }
            }
            None
        }
        serde_json::Value::Array(arr) => arr.iter().find_map(extract_first_numeric),
        _ => None,
    }
}

#[derive(Deserialize)]
struct RpcResponse<T> {
    result: Option<T>,
    error: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct PendingBlock {
    #[serde(rename = "baseFeePerGas", default)]
    base_fee_per_gas: Option<String>,
}

/// Fetch Gnosis basefee + tip via JSON-RPC. Two parallel calls:
/// `eth_getBlockByNumber("pending", false)` for the basefee and
/// `eth_maxPriorityFeePerGas` for the operator's expected tip.
pub async fn fetch_gnosis_gas(rpc_url: &str) -> Result<GasInfo, String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("bee-tui/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|e| format!("client build: {e}"))?;
    let block_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_getBlockByNumber",
        "params": ["pending", false],
    });
    let tip_body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 2,
        "method": "eth_maxPriorityFeePerGas",
        "params": [],
    });
    let (block_result, tip_result) = tokio::join!(
        post_rpc::<PendingBlock>(&client, rpc_url, &block_body),
        post_rpc::<String>(&client, rpc_url, &tip_body),
    );
    let block = block_result?;
    let base_fee_wei = match block.base_fee_per_gas {
        Some(hex) => parse_hex_u128(&hex)?,
        None => {
            return Err("eth_getBlockByNumber didn't return baseFeePerGas — is this an EIP-1559 chain?".into());
        }
    };
    let max_priority_fee_gwei = match tip_result {
        Ok(hex) => Some(parse_hex_u128(&hex)? as f64 / 1e9),
        Err(_) => None, // older RPCs skip eth_maxPriorityFeePerGas — keep going
    };
    Ok(GasInfo {
        base_fee_gwei: base_fee_wei as f64 / 1e9,
        max_priority_fee_gwei,
        fetched_at: SystemTime::now(),
        source_url: rpc_url.to_string(),
    })
}

async fn post_rpc<T: for<'de> Deserialize<'de>>(
    client: &reqwest::Client,
    url: &str,
    body: &serde_json::Value,
) -> Result<T, String> {
    let resp = client
        .post(url)
        .json(body)
        .send()
        .await
        .map_err(|e| format!("POST {url}: {e}"))?;
    if !resp.status().is_success() {
        return Err(format!("RPC returned HTTP {}", resp.status()));
    }
    let parsed: RpcResponse<T> = resp
        .json()
        .await
        .map_err(|e| format!("decode RPC response: {e}"))?;
    if let Some(err) = parsed.error {
        return Err(format!("RPC error: {err}"));
    }
    parsed
        .result
        .ok_or_else(|| "RPC response missing result field".to_string())
}

fn parse_hex_u128(hex: &str) -> Result<u128, String> {
    let s = hex.strip_prefix("0x").unwrap_or(hex);
    u128::from_str_radix(s, 16).map_err(|e| format!("parse hex {hex:?}: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_hex_u128_round_trips() {
        assert_eq!(parse_hex_u128("0x0").unwrap(), 0);
        assert_eq!(parse_hex_u128("0x10").unwrap(), 16);
        assert_eq!(parse_hex_u128("ff").unwrap(), 255);
        assert!(parse_hex_u128("not-hex").is_err());
    }

    #[test]
    fn parse_hex_handles_typical_basefee() {
        // 1 gwei = 1e9 wei = 0x3b9aca00
        let v = parse_hex_u128("0x3b9aca00").unwrap();
        assert_eq!(v, 1_000_000_000);
    }

    #[test]
    fn extract_first_numeric_walks_objects() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"a": "skip", "b": {"c": 0.42}}"#).unwrap();
        assert_eq!(extract_first_numeric(&v), Some(0.42));
    }

    #[test]
    fn extract_first_numeric_returns_none_when_missing() {
        let v: serde_json::Value =
            serde_json::from_str(r#"{"a": "x", "b": ["y", "z"]}"#).unwrap();
        assert_eq!(extract_first_numeric(&v), None);
    }

    #[test]
    fn xbzz_price_summary_includes_source() {
        let p = XbzzPrice {
            usd: 0.4321,
            fetched_at: SystemTime::now(),
            source: TOKEN_SERVICE_URL.to_string(),
        };
        let s = p.summary();
        assert!(s.contains("0.4321"));
        assert!(s.contains("tokenservice.ethswarm.org"));
    }

    #[test]
    fn gas_info_total_sums_base_and_tip() {
        let g = GasInfo {
            base_fee_gwei: 1.20,
            max_priority_fee_gwei: Some(0.50),
            fetched_at: SystemTime::now(),
            source_url: "https://rpc.example".into(),
        };
        assert!((g.total_gwei() - 1.70).abs() < 1e-9);
    }

    #[test]
    fn gas_info_summary_handles_missing_tip() {
        let g = GasInfo {
            base_fee_gwei: 2.0,
            max_priority_fee_gwei: None,
            fetched_at: SystemTime::now(),
            source_url: "rpc".into(),
        };
        assert!(g.summary().contains("tip unavailable"));
    }
}
