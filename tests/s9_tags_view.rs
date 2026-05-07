//! Snapshot tests for S9 Tags / uploads view computation.
//!
//! [`bee_tui::components::tags::Tags::view_for`] is the pure
//! function the draw path delegates to. Snapshotting it pins the
//! `TagStatus` ladder (Pending / Splitting / Pushing / Syncing /
//! Synced), the completion-pct arithmetic, the newest-first sort,
//! and the aggregate totals across the table.
//! Update via `cargo insta review` after intentional copy edits.

use std::time::Instant;

use bee::api::Tag;
use bee_tui::components::tags::Tags;
use bee_tui::watch::TagsSnapshot;

#[allow(clippy::too_many_arguments)] // test fixture helper, not API
fn tag(
    uid: u32,
    name: &str,
    total: i64,
    split: i64,
    sent: i64,
    synced: i64,
    address_byte: u8,
) -> Tag {
    Tag {
        uid,
        name: name.into(),
        total,
        split,
        seen: split,
        stored: split,
        sent,
        synced,
        address: format!("{:02x}", address_byte).repeat(32),
        started_at: "2024-05-07T12:00:00Z".into(),
    }
}

fn snapshot_with(tags: Vec<Tag>) -> TagsSnapshot {
    TagsSnapshot {
        tags,
        last_error: None,
        last_update: Some(Instant::now()),
    }
}

#[test]
fn view_empty_snapshot() {
    let view = Tags::view_for(&snapshot_with(vec![]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_pending_tag() {
    // Bee returned the tag but hasn't started counting yet (total=0).
    let view = Tags::view_for(&snapshot_with(vec![tag(
        1,
        "fresh-buy",
        0,
        0,
        0,
        0,
        0xaa,
    )]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_splitting_tag() {
    // 200 chunks expected, 80 split so far.
    let view = Tags::view_for(&snapshot_with(vec![tag(
        2,
        "movie.mp4",
        200,
        80,
        0,
        0,
        0xbb,
    )]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_pushing_tag() {
    // All split, half sent, none synced yet.
    let view = Tags::view_for(&snapshot_with(vec![tag(
        3,
        "archive.tar",
        500,
        500,
        250,
        0,
        0xcc,
    )]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_syncing_tag() {
    // Pushed but waiting on receipts: synced lags behind sent.
    let view = Tags::view_for(&snapshot_with(vec![tag(
        4,
        "doc.pdf",
        100,
        100,
        100,
        70,
        0xdd,
    )]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_synced_tag() {
    let view = Tags::view_for(&snapshot_with(vec![tag(
        5,
        "site.html",
        50,
        50,
        50,
        50,
        0xee,
    )]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_unnamed_tag_falls_back_to_uid() {
    // Bee tags can come in nameless (e.g. created via auto-tag on a
    // streaming upload). The view falls back to "tag-{uid}" so the
    // table column never shows a blank cell.
    let mut t = tag(99, "", 100, 100, 100, 100, 0xff);
    t.name = String::new();
    let view = Tags::view_for(&snapshot_with(vec![t]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_multi_tag_sorted_newest_first() {
    // Out-of-order UIDs in the input — the view should resort
    // newest-first (UID descending).
    let view = Tags::view_for(&snapshot_with(vec![
        tag(2, "older", 100, 100, 100, 100, 0xaa),
        tag(5, "newest", 100, 50, 0, 0, 0xbb),
        tag(3, "mid", 100, 100, 100, 50, 0xcc),
    ]));
    insta::assert_debug_snapshot!(view);
}

#[test]
fn view_totals_aggregate_across_tags() {
    // Three tags in different states — totals should sum split/sent/
    // synced and count `active` as non-Pending non-Synced.
    let view = Tags::view_for(&snapshot_with(vec![
        tag(1, "a", 100, 100, 100, 100, 0x01),  // Synced (not active)
        tag(2, "b", 100, 50, 0, 0, 0x02),        // Splitting (active)
        tag(3, "c", 100, 100, 100, 50, 0x03),    // Syncing (active)
        tag(4, "d", 0, 0, 0, 0, 0x04),           // Pending (not active)
    ]));
    insta::assert_debug_snapshot!(view);
}
