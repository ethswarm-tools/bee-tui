//! `:config-doctor` — read the operator's `bee.yaml`, walk a curated
//! list of deprecation / recommendation rules, and produce a report.
//!
//! Read-only: NEVER modifies the operator's config in place. The
//! report goes to a temp file (the cockpit verb) or to stdout (the
//! `--once` verb) and the operator decides what to do.
//!
//! The rule list is ported from swarm-desktop's `migration.ts`
//! (single best concentration of "deprecated Bee config keys"
//! knowledge in the ecosystem). Keys flagged here are the ones
//! recent Bee versions silently ignore or refuse — operators see
//! cryptic startup failures otherwise.
//!
//! ## Parser
//!
//! `bee.yaml` is structurally simple — top-level keys with scalar
//! values, no nested mappings for the keys this module cares about.
//! We scan line-by-line rather than pulling in a full YAML crate;
//! anything we can't classify falls through silently rather than
//! producing a false alarm. The trade-off: a key buried inside a
//! mapping won't be detected, but the deprecation list doesn't
//! contain any such keys.

use std::path::{Path, PathBuf};

/// One rule output. Each rule that the audit fires on becomes one
/// `Finding` in the report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    pub key: String,
    pub kind: FindingKind,
    /// One-line operator-facing note explaining why.
    pub note: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindingKind {
    /// Key is no longer recognised by recent Bee. Delete from
    /// `bee.yaml`.
    Deprecated,
    /// Key was renamed; carry the value over to a new key, then
    /// delete the old one.
    Renamed { to: String },
    /// Key is present but its value should be a particular setting.
    /// Used for the `storage-incentives-enable: false` /
    /// `skip-postage-snapshot: true` rules.
    ValueShouldBe { expected: String },
    /// Key is absent + we recommend setting it.
    Recommended { suggested_value: String },
}

/// Final report. Empty `findings` list means the config is clean.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Report {
    pub config_path: PathBuf,
    pub findings: Vec<Finding>,
}

impl Report {
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
    /// Multi-line operator-facing rendering, suitable for writing to
    /// a temp file or printing on stdout (in `--once` mode).
    pub fn render(&self) -> String {
        let mut out = format!(
            "# bee-tui :config-doctor report\n# config: {}\n\n",
            self.config_path.display(),
        );
        if self.findings.is_empty() {
            out.push_str("(no findings — config looks clean against the deprecation list)\n");
            return out;
        }
        out.push_str(&format!("findings: {}\n\n", self.findings.len()));
        for f in &self.findings {
            let label = match &f.kind {
                FindingKind::Deprecated => "DEPRECATED  ".to_string(),
                FindingKind::Renamed { to } => format!("RENAMED → {to}"),
                FindingKind::ValueShouldBe { expected } => {
                    format!("VALUE → {expected}")
                }
                FindingKind::Recommended { suggested_value } => {
                    format!("MISSING → {suggested_value}")
                }
            };
            out.push_str(&format!("  {label}  {}: {}\n", f.key, f.note));
        }
        out.push_str(
            "\n# bee-tui does NOT edit your bee.yaml. Apply changes by hand and restart Bee.\n",
        );
        out
    }
    /// One-line summary used by the cockpit's command-status row.
    pub fn summary(&self) -> String {
        if self.findings.is_empty() {
            "config-doctor: no findings — config looks clean".into()
        } else {
            format!(
                "config-doctor: {} finding{} — see report",
                self.findings.len(),
                if self.findings.len() == 1 { "" } else { "s" },
            )
        }
    }
}

/// Audit `bee.yaml` against the deprecation list. Returns the
/// rendered report; does not mutate the file.
pub fn audit(config_path: &Path) -> Result<Report, String> {
    let body = std::fs::read_to_string(config_path)
        .map_err(|e| format!("read {}: {e}", config_path.display()))?;
    let keys = parse_top_level_keys(&body);
    let findings = check_against_rules(&keys);
    Ok(Report {
        config_path: config_path.to_path_buf(),
        findings,
    })
}

/// Parsed top-level keys + their (string) values. Values are kept as
/// raw strings so we can do exact-string comparisons without a real
/// YAML parser. Quoted values are unquoted.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct ConfigKeys {
    pub entries: Vec<(String, String)>,
}

impl ConfigKeys {
    pub fn has(&self, key: &str) -> bool {
        self.entries.iter().any(|(k, _)| k == key)
    }
    pub fn value(&self, key: &str) -> Option<&str> {
        self.entries
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.as_str())
    }
}

/// Line-scanner: pull top-level `key: value` pairs out of a YAML
/// blob. Skips comments, blank lines, and anything that's indented.
/// Lossy for nested mappings + multiline strings, but the deprecation
/// list doesn't reference any nested key.
pub fn parse_top_level_keys(body: &str) -> ConfigKeys {
    let mut entries = Vec::new();
    for raw in body.lines() {
        // Indented (any leading whitespace) means it's nested under
        // a parent key; skip.
        if raw.starts_with(' ') || raw.starts_with('\t') {
            continue;
        }
        // Strip trailing comment (after the first ` #` we see).
        let line = raw.find(" #").map(|i| &raw[..i]).unwrap_or(raw).trim_end();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some(colon) = line.find(':') else {
            continue;
        };
        let (k, rest) = line.split_at(colon);
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let value = rest[1..]
            .trim()
            .trim_matches(|c: char| c == '"' || c == '\'');
        entries.push((key, value.to_string()));
    }
    ConfigKeys { entries }
}

/// Hard-coded deprecation list ported from swarm-desktop's
/// `migration.ts`. Each rule fires only when the relevant key is
/// present (or absent, for Recommended).
fn check_against_rules(keys: &ConfigKeys) -> Vec<Finding> {
    let mut findings = Vec::new();

    // `chain-enable` deprecated outright.
    if keys.has("chain-enable") {
        findings.push(Finding {
            key: "chain-enable".into(),
            kind: FindingKind::Deprecated,
            note: "removed in recent Bee — chain interaction is always on".into(),
        });
    }
    // `block-hash` deprecated outright.
    if keys.has("block-hash") {
        findings.push(Finding {
            key: "block-hash".into(),
            kind: FindingKind::Deprecated,
            note: "no longer used — Bee derives the genesis hash itself".into(),
        });
    }
    // `transaction` deprecated outright.
    if keys.has("transaction") {
        findings.push(Finding {
            key: "transaction".into(),
            kind: FindingKind::Deprecated,
            note: "no longer used".into(),
        });
    }
    // `swap-endpoint` renamed to `blockchain-rpc-endpoint`.
    if keys.has("swap-endpoint") {
        findings.push(Finding {
            key: "swap-endpoint".into(),
            kind: FindingKind::Renamed {
                to: "blockchain-rpc-endpoint".into(),
            },
            note: "rename and carry the URL value over".into(),
        });
    }
    // `admin-password` deprecated.
    if keys.has("admin-password") {
        findings.push(Finding {
            key: "admin-password".into(),
            kind: FindingKind::Deprecated,
            note: "Bee's admin password mechanism was removed".into(),
        });
    }
    // `debug-api-addr` deprecated.
    if keys.has("debug-api-addr") {
        findings.push(Finding {
            key: "debug-api-addr".into(),
            kind: FindingKind::Deprecated,
            note: "debug API folded into the main listener — use --debug-api-enable=true".into(),
        });
    }
    // `debug-api-enable` is recommended ON for :diagnose --pprof to
    // work. We surface this as a *recommendation* (not deletion),
    // departing from swarm-desktop which deletes the key. Operators
    // who want pprof bundles need the flag.
    match keys.value("debug-api-enable") {
        Some("true" | "True" | "TRUE") => {
            // Already enabled — nothing to flag.
        }
        Some(other) => {
            findings.push(Finding {
                key: "debug-api-enable".into(),
                kind: FindingKind::ValueShouldBe {
                    expected: "true".into(),
                },
                note: format!(
                    "currently `{other}` — set to true so :diagnose --pprof can fetch CPU profile + trace"
                ),
            });
        }
        None => {
            findings.push(Finding {
                key: "debug-api-enable".into(),
                kind: FindingKind::Recommended {
                    suggested_value: "true".into(),
                },
                note: "missing — :diagnose --pprof requires Bee's debug API to be enabled".into(),
            });
        }
    }
    // `skip-postage-snapshot` should be true on recent Bee.
    match keys.value("skip-postage-snapshot") {
        Some("true" | "True" | "TRUE") => {}
        Some(_) | None => {
            findings.push(Finding {
                key: "skip-postage-snapshot".into(),
                kind: FindingKind::ValueShouldBe {
                    expected: "true".into(),
                },
                note: "recent Bee versions ignore the postage snapshot — skipping it cuts startup time".into(),
            });
        }
    }
    // `use-postage-snapshot` should be false (the inverse of above).
    match keys.value("use-postage-snapshot") {
        Some("false" | "False" | "FALSE") | None => {}
        Some(_) => {
            findings.push(Finding {
                key: "use-postage-snapshot".into(),
                kind: FindingKind::ValueShouldBe {
                    expected: "false".into(),
                },
                note: "leave the postage snapshot off (skip-postage-snapshot=true is the canonical setting)".into(),
            });
        }
    }
    // `storage-incentives-enable` defaults to true on recent Bee, but
    // swarm-desktop pins it to false for non-staking nodes. This is
    // operator-mode-dependent; we surface it as a recommendation
    // when missing so the operator decides.
    if !keys.has("storage-incentives-enable") {
        findings.push(Finding {
            key: "storage-incentives-enable".into(),
            kind: FindingKind::Recommended {
                suggested_value: "true (or false for non-staking light nodes)".into(),
            },
            note: "set explicitly so future Bee defaults don't change your mode silently".into(),
        });
    }

    findings
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ck(entries: &[(&str, &str)]) -> ConfigKeys {
        ConfigKeys {
            entries: entries
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect(),
        }
    }

    #[test]
    fn parser_skips_comments_and_indented_lines() {
        let yaml = r#"
# top-level comment
api-addr: 0.0.0.0:1633
swap-enable: true   # inline comment

nested:
  child: ignored
  other: also-ignored

debug-api-enable: false
"#;
        let parsed = parse_top_level_keys(yaml);
        let keys: Vec<&str> = parsed.entries.iter().map(|(k, _)| k.as_str()).collect();
        assert!(keys.contains(&"api-addr"));
        assert!(keys.contains(&"swap-enable"));
        assert!(keys.contains(&"debug-api-enable"));
        assert!(!keys.contains(&"child"));
        assert!(!keys.contains(&"other"));
        // `nested:` has no value but is still a top-level key — fine.
        assert!(keys.contains(&"nested"));
    }

    #[test]
    fn parser_unquotes_values() {
        let yaml = r#"
url: "https://example.com"
single: 'foo'
plain: bar
"#;
        let parsed = parse_top_level_keys(yaml);
        assert_eq!(parsed.value("url"), Some("https://example.com"));
        assert_eq!(parsed.value("single"), Some("foo"));
        assert_eq!(parsed.value("plain"), Some("bar"));
    }

    #[test]
    fn deprecated_keys_each_trigger_a_finding() {
        let keys = ck(&[
            ("chain-enable", "true"),
            ("block-hash", "0xabc"),
            ("transaction", "0xdef"),
            ("admin-password", "hunter2"),
            ("debug-api-addr", "127.0.0.1:1635"),
            ("debug-api-enable", "true"),
            ("skip-postage-snapshot", "true"),
            ("storage-incentives-enable", "true"),
        ]);
        let findings = check_against_rules(&keys);
        let deprecated_keys: Vec<&str> = findings
            .iter()
            .filter(|f| matches!(f.kind, FindingKind::Deprecated))
            .map(|f| f.key.as_str())
            .collect();
        assert!(deprecated_keys.contains(&"chain-enable"));
        assert!(deprecated_keys.contains(&"block-hash"));
        assert!(deprecated_keys.contains(&"transaction"));
        assert!(deprecated_keys.contains(&"admin-password"));
        assert!(deprecated_keys.contains(&"debug-api-addr"));
        // No false positives for the keys we kept clean.
        assert!(!deprecated_keys.contains(&"debug-api-enable"));
        assert!(!deprecated_keys.contains(&"skip-postage-snapshot"));
    }

    #[test]
    fn swap_endpoint_renames_to_blockchain_rpc_endpoint() {
        let keys = ck(&[
            ("swap-endpoint", "https://rpc.gnosischain.com"),
            ("debug-api-enable", "true"),
            ("skip-postage-snapshot", "true"),
            ("storage-incentives-enable", "true"),
        ]);
        let findings = check_against_rules(&keys);
        let f = findings
            .iter()
            .find(|f| f.key == "swap-endpoint")
            .expect("swap-endpoint not flagged");
        match &f.kind {
            FindingKind::Renamed { to } => assert_eq!(to, "blockchain-rpc-endpoint"),
            _ => panic!("expected Renamed kind"),
        }
    }

    #[test]
    fn missing_debug_api_enable_is_recommended() {
        let keys = ck(&[
            ("skip-postage-snapshot", "true"),
            ("storage-incentives-enable", "true"),
        ]);
        let findings = check_against_rules(&keys);
        let f = findings
            .iter()
            .find(|f| f.key == "debug-api-enable")
            .expect("debug-api-enable not flagged");
        assert!(matches!(f.kind, FindingKind::Recommended { .. }));
    }

    #[test]
    fn debug_api_enable_false_is_value_should_be_finding() {
        let keys = ck(&[
            ("debug-api-enable", "false"),
            ("skip-postage-snapshot", "true"),
            ("storage-incentives-enable", "true"),
        ]);
        let findings = check_against_rules(&keys);
        let f = findings
            .iter()
            .find(|f| f.key == "debug-api-enable")
            .expect("debug-api-enable not flagged");
        match &f.kind {
            FindingKind::ValueShouldBe { expected } => assert_eq!(expected, "true"),
            _ => panic!("expected ValueShouldBe"),
        }
    }

    #[test]
    fn clean_config_produces_zero_deprecated_findings() {
        let keys = ck(&[
            ("api-addr", "0.0.0.0:1633"),
            ("data-dir", "/var/lib/bee"),
            ("password", "hunter2"),
            ("blockchain-rpc-endpoint", "https://rpc.gnosischain.com"),
            ("debug-api-enable", "true"),
            ("skip-postage-snapshot", "true"),
            ("storage-incentives-enable", "true"),
        ]);
        let findings = check_against_rules(&keys);
        // The recommendations + value-should-be entries shouldn't fire.
        assert!(findings.is_empty(), "got: {findings:#?}");
    }

    #[test]
    fn report_render_says_clean_when_no_findings() {
        let r = Report {
            config_path: PathBuf::from("/etc/bee.yaml"),
            findings: vec![],
        };
        let s = r.render();
        assert!(s.contains("clean"));
        assert!(r.is_clean());
    }

    #[test]
    fn report_render_lists_each_finding_on_its_own_line() {
        let r = Report {
            config_path: PathBuf::from("/etc/bee.yaml"),
            findings: vec![
                Finding {
                    key: "chain-enable".into(),
                    kind: FindingKind::Deprecated,
                    note: "n/a".into(),
                },
                Finding {
                    key: "swap-endpoint".into(),
                    kind: FindingKind::Renamed {
                        to: "blockchain-rpc-endpoint".into(),
                    },
                    note: "rename".into(),
                },
            ],
        };
        let s = r.render();
        assert!(s.contains("DEPRECATED"));
        assert!(s.contains("RENAMED → blockchain-rpc-endpoint"));
        assert_eq!(s.matches("chain-enable").count(), 1);
    }
}
