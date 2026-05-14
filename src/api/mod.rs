#![allow(dead_code)] // wired up in the next commit (api → app → watch).

//! Thin wrapper around [`bee::Client`] that binds it to a configured
//! [`NodeConfig`]. The cockpit talks to Bee through one [`ApiClient`]
//! per active profile.
//!
//! Construction:
//!
//! ```ignore
//! use crate::api::ApiClient;
//! let client = ApiClient::from_node(&node_config)?;
//! let rtt = client.bee().ping().await?;
//! ```
//!
//! Multi-node UX (v0.4) will hold a `HashMap<String, ApiClient>`
//! keyed by node name; the screen-level `:context` command swaps the
//! active key.

use color_eyre::eyre::{WrapErr, eyre};

use crate::config::NodeConfig;

/// User-Agent header bee-tui sends on every Bee API call. Enables
/// the Bee HTTP log-pane tab to filter bee-tui's own traffic out
/// of the "everything Bee served" view, leaving only third-party
/// clients (curl / swarm-cli / browser) visible. Format follows
/// the convention `<product>/<version>` per RFC 7231 § 5.5.3.
pub const BEE_TUI_USER_AGENT: &str = concat!("bee-tui/", env!("CARGO_PKG_VERSION"));

/// Active connection to one Bee node. Cheap to clone (`bee::Client`
/// is `Arc<Inner>` under the hood).
#[derive(Clone, Debug)]
pub struct ApiClient {
    /// Friendly profile name shown in the header bar.
    pub name: String,
    /// Resolved base URL (the scheme has been validated to be `http`
    /// or `https`).
    pub url: String,
    /// Whether the node was configured with a bearer token. We don't
    /// store the token itself — `bee::Client` keeps it.
    pub authenticated: bool,
    inner: bee::Client,
}

impl ApiClient {
    /// Build an [`ApiClient`] from a [`NodeConfig`]. Resolves the
    /// optional `@env:VAR` indirection and wires the bearer token via
    /// [`bee::Client::with_token`] when present.
    pub fn from_node(node: &NodeConfig) -> color_eyre::Result<Self> {
        let url = node.url.trim_end_matches('/').to_string();
        let token = node.resolved_token();
        let authenticated = token.is_some();
        // Build a reqwest client that stamps every outbound call with
        // bee-tui's User-Agent. Bee logs the UA on its `node/api`
        // lines (when configured to do so), which lets the cockpit's
        // Bee HTTP tab filter bee-tui's own traffic out — leaving the
        // tab as a clean view of third-party clients.
        let mut http_builder = reqwest::Client::builder().user_agent(BEE_TUI_USER_AGENT);
        // Auth is normally injected by `with_token`'s default headers.
        // Since we're handing in our own client, we replicate that
        // here so bearer auth still works on restricted-mode nodes.
        if let Some(t) = token.as_deref() {
            let mut headers = reqwest::header::HeaderMap::new();
            let value = reqwest::header::HeaderValue::from_str(&format!("Bearer {t}"))
                .map_err(|e| eyre!("invalid bearer token: {e}"))?;
            headers.insert(reqwest::header::AUTHORIZATION, value);
            http_builder = http_builder.default_headers(headers);
        }
        let http = http_builder
            .build()
            .map_err(|e| eyre!("failed to build http client: {e}"))?;
        let inner = bee::Client::with_http_client(&url, http)
            .map_err(|e| eyre!("invalid bee endpoint: {e}"))?;
        Ok(Self {
            name: node.name.clone(),
            url,
            authenticated,
            inner,
        })
    }

    /// Borrow the underlying [`bee::Client`] for direct API calls.
    pub fn bee(&self) -> &bee::Client {
        &self.inner
    }

    /// Health round-trip latency. Convenience over `self.bee().ping()`
    /// for the connection-status indicator.
    pub async fn ping(&self) -> color_eyre::Result<std::time::Duration> {
        self.inner
            .ping()
            .await
            .wrap_err_with(|| format!("ping {} ({}) failed", self.name, self.url))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node(url: &str, token: Option<&str>) -> NodeConfig {
        NodeConfig {
            name: "test".into(),
            url: url.into(),
            token: token.map(String::from),
            log_file: None,
            log_command: None,
            default: false,
        }
    }

    #[test]
    fn from_node_strips_trailing_slash() {
        let c = ApiClient::from_node(&node("http://localhost:1633/", None)).unwrap();
        assert_eq!(c.url, "http://localhost:1633");
        assert!(!c.authenticated);
    }

    #[test]
    fn from_node_with_token_marks_authenticated() {
        let c = ApiClient::from_node(&node("http://localhost:1633", Some("dummy-jwt"))).unwrap();
        assert!(c.authenticated);
    }

    #[test]
    fn from_node_rejects_bogus_url() {
        let err = ApiClient::from_node(&node("not a url", None)).unwrap_err();
        assert!(format!("{err}").contains("invalid"));
    }
}
