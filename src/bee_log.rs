//! Parser for Bee's logfmt-with-quoted-keys log format. Each Bee
//! log line looks like:
//!
//! ```text
//! "time"="2026-05-07 22:14:31.211867" "level"="debug" "logger"="node/batchservice" "msg"="block height updated" "new_block"=10809557
//! ```
//!
//! Always-quoted keys, mixed quoted-string / unquoted-scalar values,
//! single space separator. The format is non-standard enough that
//! reaching for a generic logfmt crate would either over-parse or
//! under-parse — a small purpose-built scanner does the right thing
//! and stays testable.
//!
//! The output of [`parse_line`] is a [`BeeLogEntry`] with the four
//! "structural" fields (time / level / logger / msg) lifted out and
//! everything else captured as `(key, value)` pairs preserving order.
//!
//! ## What this module is not
//!
//! Not a tailer — that's `bee_log_tailer.rs`. Not a renderer — the
//! `LogPane` consumes [`BeeLogLine`]s built from these entries.
//! Pure parsing only.

use crate::components::log_pane::{BeeLogLine, LogTab};

/// One parsed Bee log line. Owned strings — the input is consumed
/// once per call and we don't try to be zero-copy.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct BeeLogEntry {
    /// `time` field. Bee writes UTC with microsecond precision:
    /// `"2026-05-07 22:14:31.211867"`. We keep it verbatim so the
    /// renderer doesn't have to re-parse / re-format.
    pub time: String,
    /// `level` field, lower-cased. `error`, `warning`, `info`,
    /// `debug`, occasionally `none` / `trace`. Empty if the line
    /// didn't have a `level` field.
    pub level: String,
    /// `logger` field. Usually `node/<subsystem>` — `node/pseudosettle`,
    /// `node/kademlia`, `node/api`. Empty if absent.
    pub logger: String,
    /// `msg` field. Empty if absent (rare — Bee always logs a msg).
    pub msg: String,
    /// All other fields, in the order they appeared. Preserved so
    /// the renderer can show contextual data (peer addresses,
    /// amounts, error reasons) verbatim.
    pub extras: Vec<(String, String)>,
}

impl BeeLogEntry {
    /// Lines coming from Bee's REST API server have logger names
    /// that start with `node/api` (covers `node/api`,
    /// `node/api/access`, and similar variants across Bee versions).
    /// They get their own tab so served-request traffic doesn't
    /// drown out the severity views.
    ///
    /// Limitation: bee-tui's *own* requests against Bee also produce
    /// these lines on the server side. There's no reliable way to
    /// filter them out from Bee's perspective (User-Agent isn't in
    /// the structured fields). The cockpit's bee::http tab — fed
    /// from bee-tui's own client tracing — is the better place to
    /// see "what bee-tui called"; this tab is "everything Bee
    /// served", which usually overlaps but doesn't have to.
    pub fn is_bee_http(&self) -> bool {
        self.logger.starts_with("node/api")
    }

    /// Map the `level` + logger combination to the cockpit tab it
    /// belongs on. The Bee-HTTP check wins over severity routing —
    /// an `error`-level line from `node/api` shows up on Bee HTTP,
    /// not on Errors. (Reason: an operator looking at Errors wants
    /// to see *infrastructure* errors, not "client sent a malformed
    /// request" lines that flood under load testing.) Returns
    /// `None` for unrecognised levels so a future Bee build with a
    /// new severity isn't silently misfiled.
    pub fn tab(&self) -> Option<LogTab> {
        if self.is_bee_http() {
            return Some(LogTab::BeeHttp);
        }
        match self.level.as_str() {
            "error" | "err" | "fatal" => Some(LogTab::Errors),
            "warning" | "warn" => Some(LogTab::Warning),
            "info" => Some(LogTab::Info),
            "debug" | "trace" => Some(LogTab::Debug),
            _ => None,
        }
    }

    /// Build the renderable form for [`LogPane::push_bee`]. Combines
    /// the structural fields and extras into the three-column shape
    /// the tab renderer expects (timestamp / logger / message).
    pub fn to_log_line(&self) -> BeeLogLine {
        let mut message = self.msg.clone();
        for (k, v) in &self.extras {
            // Compact `key=value` format mirroring Bee's own logfmt
            // shape, minus the redundant outer quotes around keys.
            // Operators reading the tail recognise the layout.
            if !message.is_empty() {
                message.push(' ');
            }
            message.push_str(k);
            message.push('=');
            // Re-quote values that contain spaces; bare otherwise.
            if v.chars().any(|c| c == ' ' || c == '"') || v.is_empty() {
                message.push('"');
                message.push_str(v);
                message.push('"');
            } else {
                message.push_str(v);
            }
        }
        BeeLogLine {
            timestamp: self.time.clone(),
            logger: self.logger.clone(),
            message,
        }
    }
}

/// Parse a single Bee log line. Returns `None` for empty / unparseable
/// input — these are dropped silently in the live tail so a single
/// malformed line doesn't break the stream.
pub fn parse_line(line: &str) -> Option<BeeLogEntry> {
    let line = line.trim();
    if line.is_empty() {
        return None;
    }

    let mut entry = BeeLogEntry::default();
    let mut cursor = line;

    while !cursor.is_empty() {
        cursor = cursor.trim_start();
        if cursor.is_empty() {
            break;
        }
        // Each pair is "key"=value. Key is always quoted in Bee's
        // format; we still tolerate a bare key in case the format
        // shifts in a future version.
        let (key, rest) = take_key(cursor)?;
        let after_eq = rest.strip_prefix('=')?;
        let (value, rest) = take_value(after_eq)?;
        match key.as_str() {
            "time" => entry.time = value,
            "level" => entry.level = value.to_ascii_lowercase(),
            "logger" => entry.logger = value,
            "msg" => entry.msg = value,
            _ => entry.extras.push((key, value)),
        }
        cursor = rest;
    }

    // A line without any of the structural fields isn't a Bee log
    // entry — could be a stray banner or panic line. Preserving these
    // would muddy the severity tabs, so reject them.
    if entry.time.is_empty() && entry.level.is_empty() && entry.logger.is_empty() {
        return None;
    }
    Some(entry)
}

/// Pull a key off the front of `s`. Bee always quotes keys; we also
/// accept bare identifiers `[a-zA-Z_][a-zA-Z0-9_-]*` for resilience.
/// Returns `(key, remainder)` or `None` if the input doesn't start
/// with a valid key.
fn take_key(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        let key = rest[..end].to_string();
        Some((key, &rest[end + 1..]))
    } else {
        // Bare key — read until '=' or whitespace. Empty bare key is
        // treated as a parse failure.
        let end = s
            .find(|c: char| c == '=' || c.is_whitespace())
            .unwrap_or(s.len());
        if end == 0 {
            return None;
        }
        Some((s[..end].to_string(), &s[end..]))
    }
}

/// Pull a value off the front of `s`. Quoted values (the common
/// case) consume up to the closing quote; unquoted values (numbers,
/// booleans) read until whitespace or end of input.
fn take_value(s: &str) -> Option<(String, &str)> {
    if let Some(rest) = s.strip_prefix('"') {
        let end = rest.find('"')?;
        let value = rest[..end].to_string();
        Some((value, &rest[end + 1..]))
    } else {
        let end = s.find(char::is_whitespace).unwrap_or(s.len());
        Some((s[..end].to_string(), &s[end..]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_pseudosettle_debug_line() {
        // Sourced verbatim from the operator's live testnet log.
        let line = r#""time"="2026-05-07 22:14:19.605485" "level"="debug" "logger"="node/pseudosettle" "v"=1 "msg"="pseudosettle sending payment message to peer" "peer_address"="097b3be6af660b6d9569c47f1f077ed419e5326f6ab4930c587b2f6a1cdada55" "amount"="48870000""#;
        let e = parse_line(line).expect("must parse");
        assert_eq!(e.time, "2026-05-07 22:14:19.605485");
        assert_eq!(e.level, "debug");
        assert_eq!(e.logger, "node/pseudosettle");
        assert_eq!(e.msg, "pseudosettle sending payment message to peer");
        // Extras preserve order.
        assert_eq!(e.extras[0], ("v".into(), "1".into()));
        assert_eq!(
            e.extras[1],
            (
                "peer_address".into(),
                "097b3be6af660b6d9569c47f1f077ed419e5326f6ab4930c587b2f6a1cdada55".into()
            )
        );
        assert_eq!(e.extras[2], ("amount".into(), "48870000".into()));
        assert_eq!(e.tab(), Some(LogTab::Debug));
    }

    #[test]
    fn parses_unquoted_numeric_value() {
        // Bee writes integers without quotes: "v"=1, "new_block"=10809557.
        let line = r#""time"="t" "level"="debug" "logger"="node/batchservice" "msg"="block height updated" "new_block"=10809557"#;
        let e = parse_line(line).expect("must parse");
        assert_eq!(e.extras, vec![("new_block".into(), "10809557".into())]);
    }

    #[test]
    fn parses_unquoted_bool_value() {
        let line = r#""time"="t" "level"="debug" "logger"="node" "msg"="sync status check" "synced"=false "reserveSize"=2582243"#;
        let e = parse_line(line).expect("must parse");
        assert_eq!(e.extras[0], ("synced".into(), "false".into()));
        assert_eq!(e.extras[1], ("reserveSize".into(), "2582243".into()));
    }

    #[test]
    fn parses_unquoted_float_value() {
        // Bee mixes integer + float numerics in the same line.
        let line = r#""time"="t" "level"="debug" "logger"="node" "msg"="sync status check" "syncRate"=0.0989528913580248"#;
        let e = parse_line(line).expect("must parse");
        assert_eq!(
            e.extras[0],
            ("syncRate".into(), "0.0989528913580248".into())
        );
    }

    #[test]
    fn parses_long_error_message() {
        // The libp2p stream-reset error is the longest single value
        // we've seen in the wild — make sure we don't truncate.
        let line = r#""time"="t" "level"="debug" "logger"="node/libp2p" "msg"="handle protocol failed" "protocol"="swap" "version"="1.0.0" "stream"="swap" "peer"="54b5..." "error"="read request from peer 54b5...: stream reset (remote): code: 0x0: transport error: stream reset by remote, error code: 0""#;
        let e = parse_line(line).expect("must parse");
        let err_pair = e.extras.iter().find(|(k, _)| k == "error").unwrap();
        assert!(err_pair.1.contains("stream reset by remote"));
    }

    #[test]
    fn level_routing_covers_known_severities() {
        for (lvl, tab) in [
            ("error", LogTab::Errors),
            ("err", LogTab::Errors),
            ("fatal", LogTab::Errors),
            ("warning", LogTab::Warning),
            ("warn", LogTab::Warning),
            ("info", LogTab::Info),
            ("debug", LogTab::Debug),
            ("trace", LogTab::Debug),
        ] {
            let e = BeeLogEntry {
                level: lvl.into(),
                ..Default::default()
            };
            assert_eq!(e.tab(), Some(tab), "level {lvl} should route to {tab:?}");
        }
    }

    #[test]
    fn level_routing_unknown_returns_none() {
        // Defensive — a future Bee build that adds a new severity
        // shouldn't get silently slotted into the wrong tab.
        let e = BeeLogEntry {
            level: "panic".into(),
            ..Default::default()
        };
        assert_eq!(e.tab(), None);
        let e = BeeLogEntry::default();
        assert_eq!(e.tab(), None);
    }

    #[test]
    fn node_api_logger_routes_to_bee_http() {
        // Bee's REST API server uses `node/api` as the logger name.
        // Should land on the Bee HTTP tab regardless of severity.
        for logger in ["node/api", "node/api/access", "node/api/handler"] {
            let e = BeeLogEntry {
                logger: logger.into(),
                level: "debug".into(),
                ..Default::default()
            };
            assert_eq!(e.tab(), Some(LogTab::BeeHttp), "logger {logger}");
        }
    }

    #[test]
    fn bee_http_wins_over_severity_routing() {
        // An error-level line from node/api goes to BeeHttp, not
        // Errors — the spec says "errors" is for infrastructure
        // problems, not 4xx replies to clients.
        let e = BeeLogEntry {
            logger: "node/api".into(),
            level: "error".into(),
            ..Default::default()
        };
        assert_eq!(e.tab(), Some(LogTab::BeeHttp));
    }

    #[test]
    fn non_api_logger_falls_through_to_severity() {
        // Sanity check the regression: the logger filter should
        // ONLY catch `node/api*`, not anything that happens to
        // contain `api`.
        let e = BeeLogEntry {
            logger: "node/batchapi".into(),
            level: "error".into(),
            ..Default::default()
        };
        assert_eq!(e.tab(), Some(LogTab::Errors));
    }

    #[test]
    fn level_is_lowercased_during_parse() {
        let line = r#""time"="t" "level"="ERROR" "logger"="node" "msg"="oops""#;
        let e = parse_line(line).expect("must parse");
        assert_eq!(e.level, "error");
        assert_eq!(e.tab(), Some(LogTab::Errors));
    }

    #[test]
    fn empty_input_returns_none() {
        assert!(parse_line("").is_none());
        assert!(parse_line("   ").is_none());
        assert!(parse_line("\n").is_none());
    }

    #[test]
    fn line_without_structural_fields_returns_none() {
        // Stray banner or panic line that doesn't fit the format
        // shouldn't pollute the severity tabs. parse_line drops it.
        assert!(parse_line(r#""foo"="bar" "baz"=42"#).is_none());
    }

    #[test]
    fn malformed_line_returns_none() {
        // No `=` after the key — total parse failure, drop.
        assert!(parse_line(r#""time" "level"="debug""#).is_none());
        // Unterminated quoted value — parse failure, drop.
        assert!(parse_line(r#""time"="2026" "level"="debug"#).is_none());
    }

    #[test]
    fn to_log_line_compacts_extras_into_message() {
        let e = BeeLogEntry {
            time: "t1".into(),
            logger: "node/foo".into(),
            msg: "did a thing".into(),
            extras: vec![("count".into(), "42".into()), ("peer".into(), "abc".into())],
            ..Default::default()
        };
        let line = e.to_log_line();
        assert_eq!(line.timestamp, "t1");
        assert_eq!(line.logger, "node/foo");
        assert_eq!(line.message, "did a thing count=42 peer=abc");
    }

    #[test]
    fn to_log_line_quotes_values_with_spaces() {
        let e = BeeLogEntry {
            msg: "x".into(),
            extras: vec![("error".into(), "stream reset by remote".into())],
            ..Default::default()
        };
        let line = e.to_log_line();
        assert!(line.message.contains(r#"error="stream reset by remote""#));
    }

    #[test]
    fn to_log_line_quotes_empty_values() {
        // Without quoting, an empty value would render as `key=`
        // which is ambiguous (key alone vs. key with empty value).
        let e = BeeLogEntry {
            msg: "x".into(),
            extras: vec![("nullable".into(), "".into())],
            ..Default::default()
        };
        let line = e.to_log_line();
        assert!(line.message.contains(r#"nullable="""#));
    }
}
