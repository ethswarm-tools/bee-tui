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
        let inner = match token.as_deref() {
            Some(t) => bee::Client::with_token(&url, t)
                .map_err(|e| eyre!("invalid bee endpoint or token: {e}"))?,
            None => bee::Client::new(&url).map_err(|e| eyre!("invalid bee endpoint: {e}"))?,
        };
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
