//! Snapshot tests for the S2 Stamps drill pane.
//!
//! [`bee_tui::components::stamps::Stamps::compute_drill_view`] takes
//! a `PostageBatchBuckets` and produces the histogram + worst-N
//! summary the drill pane renders. Pinning it via insta locks the
//! bin distribution and worst-bucket ordering against future
//! refactors.

use bee::postage::{BatchBucket, PostageBatchBuckets};
use bee_tui::components::stamps::Stamps;

fn buckets_with(counts: &[(u32, u32)], depth: u8, bucket_depth: u8) -> PostageBatchBuckets {
    let upper_bound = 1u32 << (depth - bucket_depth);
    let buckets = counts
        .iter()
        .map(|(id, c)| BatchBucket {
            bucket_id: *id,
            collisions: *c,
        })
        .collect();
    PostageBatchBuckets {
        depth,
        bucket_depth,
        bucket_upper_bound: upper_bound,
        buckets,
    }
}

#[test]
fn drill_view_realistic_skewed_batch() {
    // Real operator-relevant shape: a couple of nearly-full buckets,
    // a dozen mid, hundreds empty. Reproduces the bee#5334 scenario
    // where average usage is low but a single bucket is the upload
    // ceiling.
    let mut entries: Vec<(u32, u32)> = vec![
        (3, 64),   // 100 %
        (17, 63),  // 98 %
        (101, 60), // 93 %
        (200, 59), // 92 %
        (455, 50), // 78 %
        (456, 48), // 75 %
        (457, 32), // 50 %
        (612, 30), // 46 %
        (700, 5),  // 7 %
        (701, 4),  // 6 %
        (702, 3),  // 4 %
        (703, 2),  // 3 %
        (704, 1),  // 1 %
    ];
    // Pad with 100 empty buckets so the 0 % bin dominates the
    // distribution — matches the real-world shape Bee returns.
    for i in 0..100u32 {
        entries.push((10_000 + i, 0));
    }
    let view = Stamps::compute_drill_view(&buckets_with(&entries, 22, 16), None);
    insta::assert_debug_snapshot!(view);
}

#[test]
fn drill_view_full_batch() {
    // Pathological: one bucket has saturated, no other activity.
    let view = Stamps::compute_drill_view(
        &buckets_with(&[(7, 64), (8, 0), (9, 0), (10, 0)], 22, 16),
        None,
    );
    insta::assert_debug_snapshot!(view);
}

#[test]
fn drill_view_empty_batch() {
    // Fresh buy — every bucket is at zero.
    let entries: Vec<(u32, u32)> = (0..16u32).map(|i| (i, 0)).collect();
    let view = Stamps::compute_drill_view(&buckets_with(&entries, 22, 16), None);
    insta::assert_debug_snapshot!(view);
}
