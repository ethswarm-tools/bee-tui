//! Async walker for Mantaray manifests.
//!
//! Bee-rs 1.6 exposes [`bee::manifest::MantarayNode`] +
//! [`bee::manifest::unmarshal`] but no recursive-load API; this
//! module implements lazy fork-loading on top of the chunk-download
//! primitive. Each call fetches a single chunk and parses it into a
//! `MantarayNode` (or surfaces a recoverable error when the data is
//! not a valid manifest, mirroring the swarm-cli pattern).
//!
//! The S12 Manifests screen owns the tree-walk state machine (which
//! forks the operator expanded, per-fork load status, cursor position).
//! This module is the I/O leaf — pure-fn-ish: takes a reference, hits
//! the network, returns a parsed node or an error.

use std::sync::Arc;

use bee::manifest::{MantarayNode, unmarshal};
use bee::swarm::Reference;

use crate::api::ApiClient;

/// Outcome of inspecting a reference. Distinguishes "this parses as a
/// manifest" from "this is a raw chunk we couldn't parse" so the
/// caller can route a `:inspect` invocation correctly.
#[derive(Debug, Clone)]
pub enum InspectResult {
    /// The chunk parsed as a Mantaray manifest. `node` is the parsed
    /// root; `bytes_len` is the raw chunk size for display.
    Manifest { node: Box<MantarayNode>, bytes_len: usize },
    /// The chunk fetched but didn't parse as a manifest. We surface
    /// the size so the caller can display "raw, X bytes".
    RawChunk { bytes_len: usize },
    /// The fetch itself failed (network or 404). String is the error
    /// for display.
    Error(String),
}

/// Fetch a Mantaray chunk by reference and parse it. Returns the
/// parsed node on success; on parse-failure surfaces the underlying
/// error string ("mantaray: invalid version hash" etc).
pub async fn load_node(
    api: Arc<ApiClient>,
    reference: Reference,
) -> Result<MantarayNode, String> {
    let bytes = api
        .bee()
        .file()
        .download_chunk(&reference, None)
        .await
        .map_err(|e| format!("download_chunk: {e}"))?;
    unmarshal(&bytes, reference.as_bytes()).map_err(|e| format!("unmarshal: {e}"))
}

/// Universal "what is this thing?" probe. Fetches the chunk; tries
/// `MantarayNode::unmarshal`; on parse-success returns
/// `InspectResult::Manifest`, on parse-failure returns
/// `InspectResult::RawChunk` (the operator's chunk is real but isn't
/// a manifest — just a raw blob).
pub async fn inspect(api: Arc<ApiClient>, reference: Reference) -> InspectResult {
    let bytes = match api.bee().file().download_chunk(&reference, None).await {
        Ok(b) => b,
        Err(e) => return InspectResult::Error(format!("download_chunk: {e}")),
    };
    let len = bytes.len();
    match unmarshal(&bytes, reference.as_bytes()) {
        Ok(node) => InspectResult::Manifest {
            node: Box::new(node),
            bytes_len: len,
        },
        Err(_) => InspectResult::RawChunk { bytes_len: len },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inspect_result_variants_compile_and_clone() {
        // Smoke test: the enum's variant shape is what callers expect
        // and `Clone` is correctly derived (the screen state copies
        // these around). Doesn't exercise the network path; that
        // lives in integration tests.
        let raw = InspectResult::RawChunk { bytes_len: 4096 };
        let raw2 = raw.clone();
        match raw2 {
            InspectResult::RawChunk { bytes_len } => assert_eq!(bytes_len, 4096),
            _ => panic!("variant mismatch"),
        }
        let err = InspectResult::Error("net fail".into());
        let err2 = err.clone();
        match err2 {
            InspectResult::Error(s) => assert_eq!(s, "net fail"),
            _ => panic!("variant mismatch"),
        }
    }
}
