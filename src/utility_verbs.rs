//! Pure-local utility verbs.
//!
//! Each verb does its work entirely on the operator's machine — no
//! Bee API call, no chain RPC, no network — by composing primitives
//! that bee-rs already exposes for offline use. They exist to absorb
//! the small one-off "what's the hash of this", "what's the CID of
//! that", "what would batch depth N hold?" questions that today push
//! operators out to swarm-cli or bee-js.
//!
//! Five verbs ship in v1.2:
//!   * `:hash <path>`              — Swarm reference of a local file/dir
//!   * `:cid <ref> [manifest|feed]` — encode reference as a multibase CID
//!   * `:depth-table`              — canonical depth → capacity table
//!   * `:gsoc-mine <overlay> <id>` — mine a GSOC signer for a target
//!   * `:pss-target <overlay>`     — derive `Utils.makeMaxTarget` prefix
//!
//! `:feed-url <owner> <topic>` is intentionally deferred to v1.3+: the
//! pure-local prediction of a feed-manifest URL needs a bee-rs primitive
//! (`feed::manifest_address(owner, topic)`) that 1.6 does not export.
//! Once bee-rs adds it, a sibling fn `feed_url_for(owner, topic)` lands
//! here.

use std::fmt::Write;
use std::path::Path;

use bee::file::hash_directory;
use bee::postage::{
    EFFECTIVE_SIZE_BREAKPOINTS, get_stamp_effective_bytes, get_stamp_theoretical_bytes,
};
use bee::swarm::{
    CidType, FileChunker, GSOC_DEFAULT_PROXIMITY, Identifier, Reference, convert_reference_to_cid,
    gsoc_mine,
};

/// Bee accepts at most 4 hex chars (2 bytes) of PSS target prefix —
/// matches `bee-js` `PSS_TARGET_HEX_LENGTH_MAX`.
const PSS_TARGET_HEX_LENGTH_MAX: usize = 4;

/// `:hash <path>` — content-addressed Swarm reference of a local file
/// or directory, computed entirely offline. Mirrors `swarm-cli hash`.
///
/// Directory inputs go through `hash_directory` which produces the
/// same Mantaray collection root a `bee` upload would. Single-file
/// inputs go through a streaming `FileChunker` so very large files
/// don't allocate a second copy of their bytes.
pub fn hash_path(path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let meta = std::fs::metadata(p).map_err(|e| format!("stat {path:?}: {e}"))?;
    if meta.is_dir() {
        let r = hash_directory(p).map_err(|e| format!("hash_directory: {e}"))?;
        return Ok(r.to_hex());
    }
    let bytes = std::fs::read(p).map_err(|e| format!("read {path:?}: {e}"))?;
    let mut chunker = FileChunker::new();
    chunker
        .write(&bytes)
        .map_err(|e| format!("chunker write: {e}"))?;
    let root = chunker
        .finalize()
        .map_err(|e| format!("chunker finalize: {e}"))?;
    Ok(root.address.to_hex())
}

/// `:cid <ref> [manifest|feed]` — re-encode a 32-byte Swarm reference
/// as a multibase CIDv1 string (`bah5...`). The kind defaults to
/// `manifest`. Encrypted (64-byte) refs are rejected because the CID
/// envelope can't hold them.
pub fn cid_for_ref(ref_hex: &str, kind: CidType) -> Result<String, String> {
    let r = Reference::from_hex(ref_hex.trim()).map_err(|e| format!("ref parse: {e}"))?;
    convert_reference_to_cid(&r, kind).map_err(|e| format!("cid encode: {e}"))
}

/// Parse the optional `manifest|feed` second arg. Defaults to manifest
/// when absent.
pub fn parse_cid_kind(arg: Option<&str>) -> Result<CidType, String> {
    match arg.unwrap_or("manifest").to_ascii_lowercase().as_str() {
        "manifest" | "m" => Ok(CidType::Manifest),
        "feed" | "f" => Ok(CidType::Feed),
        other => Err(format!(
            "unknown CID kind: {other:?} (expected 'manifest' or 'feed')"
        )),
    }
}

/// `:depth-table` — print the canonical depth → effective-bytes lookup
/// the rest of the cockpit's economics math is anchored on. Numbers
/// come from bee-rs `EFFECTIVE_SIZE_BREAKPOINTS` so they stay in sync
/// with whatever the runtime truth is.
pub fn depth_table() -> String {
    let mut out = String::new();
    out.push_str("depth   theoretical              effective    eff %\n");
    out.push_str("-----  ------------    -------------------    -----\n");
    for (depth, _gb) in EFFECTIVE_SIZE_BREAKPOINTS {
        let theoretical = get_stamp_theoretical_bytes(*depth);
        let effective = get_stamp_effective_bytes(*depth);
        let pct = if theoretical > 0 {
            (effective as f64) / (theoretical as f64) * 100.0
        } else {
            0.0
        };
        let _ = writeln!(
            &mut out,
            "{depth:>5}  {:>12}    {:>19}    {:>4.1}%",
            human_bytes(theoretical),
            human_bytes(effective),
            pct,
        );
    }
    out
}

/// `:gsoc-mine <overlay> <identifier>` — find a `PrivateKey` whose SOC
/// at `(identifier, owner_of_priv_key)` lands within
/// `GSOC_DEFAULT_PROXIMITY` bits of the target overlay. Pure CPU work,
/// no network. Result is the hex private key plus the proximity that
/// was actually achieved (≥ default).
pub fn gsoc_mine_for(overlay_hex: &str, identifier_str: &str) -> Result<String, String> {
    let overlay = parse_hex_32(overlay_hex)?;
    let id = Identifier::from_string(identifier_str);
    let pk = gsoc_mine(&overlay, &id, GSOC_DEFAULT_PROXIMITY)
        .map_err(|e| format!("gsoc_mine: {e}"))?;
    Ok(format!(
        "private_key=0x{}\nproximity≥{} (default)",
        pk.to_hex(),
        GSOC_DEFAULT_PROXIMITY,
    ))
}

/// `:pss-target <overlay>` — Bee's `/pss/send` accepts at most a
/// 4-hex-char (2-byte) target prefix; this verb returns those four
/// chars from a full overlay so dApp authors don't have to re-derive
/// the rule by hand. Mirrors bee-js `Utils.makeMaxTarget`.
pub fn pss_target_for(overlay_hex: &str) -> Result<String, String> {
    let cleaned = overlay_hex.trim().trim_start_matches("0x");
    if cleaned.len() < PSS_TARGET_HEX_LENGTH_MAX {
        return Err(format!(
            "overlay hex too short: need ≥{PSS_TARGET_HEX_LENGTH_MAX} chars, got {}",
            cleaned.len()
        ));
    }
    if !cleaned.chars().all(|c| c.is_ascii_hexdigit()) {
        return Err("overlay must be hex".into());
    }
    Ok(cleaned[..PSS_TARGET_HEX_LENGTH_MAX].to_ascii_lowercase())
}

fn parse_hex_32(s: &str) -> Result<[u8; 32], String> {
    let cleaned = s.trim().trim_start_matches("0x");
    if cleaned.len() != 64 {
        return Err(format!(
            "expected 64 hex chars (32 bytes), got {}",
            cleaned.len()
        ));
    }
    let mut out = [0u8; 32];
    for i in 0..32 {
        let byte = u8::from_str_radix(&cleaned[2 * i..2 * i + 2], 16)
            .map_err(|e| format!("hex char {}: {e}", 2 * i))?;
        out[i] = byte;
    }
    Ok(out)
}

fn human_bytes(n: i64) -> String {
    const KIB: i64 = 1 << 10;
    const MIB: i64 = 1 << 20;
    const GIB: i64 = 1 << 30;
    const TIB: i64 = 1 << 40;
    if n >= TIB {
        format!("{:.2} TiB", n as f64 / TIB as f64)
    } else if n >= GIB {
        format!("{:.2} GiB", n as f64 / GIB as f64)
    } else if n >= MIB {
        format!("{:.2} MiB", n as f64 / MIB as f64)
    } else if n >= KIB {
        format!("{:.2} KiB", n as f64 / KIB as f64)
    } else {
        format!("{n} B")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn depth_table_renders_each_breakpoint_row() {
        let out = depth_table();
        for (depth, _) in EFFECTIVE_SIZE_BREAKPOINTS {
            assert!(
                out.contains(&format!("{depth:>5}")),
                "depth {depth} missing from rendered table:\n{out}"
            );
        }
        // Header still in place.
        assert!(out.contains("theoretical"));
        assert!(out.contains("effective"));
    }

    #[test]
    fn cid_round_trips_a_known_reference() {
        let ref_hex = "0".repeat(64);
        let cid = cid_for_ref(&ref_hex, CidType::Manifest).expect("encode");
        assert!(cid.starts_with('b'), "CID must use multibase prefix b");
    }

    #[test]
    fn cid_kind_parses_aliases() {
        assert!(matches!(parse_cid_kind(None), Ok(CidType::Manifest)));
        assert!(matches!(
            parse_cid_kind(Some("manifest")),
            Ok(CidType::Manifest)
        ));
        assert!(matches!(parse_cid_kind(Some("M")), Ok(CidType::Manifest)));
        assert!(matches!(parse_cid_kind(Some("feed")), Ok(CidType::Feed)));
        assert!(matches!(parse_cid_kind(Some("f")), Ok(CidType::Feed)));
        assert!(parse_cid_kind(Some("garbage")).is_err());
    }

    #[test]
    fn cid_rejects_short_hex() {
        let err = cid_for_ref("deadbeef", CidType::Manifest).unwrap_err();
        assert!(err.contains("ref parse"), "got: {err}");
    }

    #[test]
    fn pss_target_returns_first_four_hex_lowercased() {
        let overlay = "AABBCCDDeeff00112233445566778899aabbccddeeff00112233445566778899";
        let out = pss_target_for(overlay).unwrap();
        assert_eq!(out, "aabb");
    }

    #[test]
    fn pss_target_strips_0x_prefix() {
        let overlay = "0xaabbccddeeff00112233445566778899aabbccddeeff00112233445566778899";
        let out = pss_target_for(overlay).unwrap();
        assert_eq!(out, "aabb");
    }

    #[test]
    fn pss_target_rejects_short_input() {
        let err = pss_target_for("aa").unwrap_err();
        assert!(err.contains("too short"), "got: {err}");
    }

    #[test]
    fn pss_target_rejects_non_hex() {
        let err = pss_target_for("zzzz_not_hex").unwrap_err();
        assert!(err.contains("hex"), "got: {err}");
    }

    #[test]
    fn parse_hex_32_round_trips_zero_overlay() {
        let hex = "0".repeat(64);
        let arr = parse_hex_32(&hex).unwrap();
        assert_eq!(arr, [0u8; 32]);
    }

    #[test]
    fn parse_hex_32_rejects_wrong_length() {
        assert!(parse_hex_32("aa").is_err());
        assert!(parse_hex_32(&"f".repeat(63)).is_err());
        assert!(parse_hex_32(&"f".repeat(65)).is_err());
    }

    #[test]
    fn human_bytes_picks_the_right_unit() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(2048), "2.00 KiB");
        assert_eq!(human_bytes(2 * 1024 * 1024), "2.00 MiB");
        assert_eq!(human_bytes(3 * 1024 * 1024 * 1024), "3.00 GiB");
    }

    #[test]
    fn hash_path_round_trips_a_temp_file() {
        let tmp = std::env::temp_dir().join("bee-tui-utility-verbs-hash-test.bin");
        let payload = b"hello bee-tui";
        std::fs::write(&tmp, payload).unwrap();
        let r = hash_path(tmp.to_str().unwrap()).expect("hash_path");
        assert_eq!(r.len(), 64, "ref hex must be 32 bytes ({r})");
        // Same content → same ref. Run again, expect identical output.
        let r2 = hash_path(tmp.to_str().unwrap()).unwrap();
        assert_eq!(r, r2);
        let _ = std::fs::remove_file(tmp);
    }

    #[test]
    fn hash_path_errors_on_missing_file() {
        let err = hash_path("/definitely/not/a/path/anywhere/u8s9d8f").unwrap_err();
        assert!(err.contains("stat"), "got: {err}");
    }
}
