# S9 — Tags / uploads

One row per Bee tag. Bee creates a tag for every upload (and
exposes them via `/tags`); the row shows the lifecycle stage
plus per-stage progress so operators see exactly where an
upload is — splitting, pushing, syncing, or stalled.

## Why this screen exists

Operators uploading large content (a 4 GiB tarball, a
directory of files, a feed update) need to know when the
upload is "done enough" to share. Bee's tag tracks five
counters per upload:

- `total` — total chunks declared up front
- `split` — chunks produced by the splitter
- `seen` — chunks the network already had (no push needed)
- `stored` — chunks landed locally
- `sent` — chunks pushed to the network
- `synced` — chunks the network confirmed receipt for

Bee's `/tags` returns all of these, but operators routinely
focus on the wrong one. `synced == total` is the only
correct "done" check. `stored == total` only means *you*
have the chunks; the network still needs them.

S9 surfaces all the stages so the lifecycle is visible at a
glance.

## The list view

```
  UID    LABEL              ADDRESS         STATUS       %     SYNCED / TOTAL
  142    backup-2026-05     0xabcd…ef       ✓ synced    100   8,192 / 8,192
  143    site-publish       0xdeadb…ef      ▒ pushing    74   1,247 / 1,684
  144    streaming-feed     —               · pending     0       0 / 0
  145    deep-archive       0xc0ffee…00     ▒ syncing    91   3,421 / 3,765
```

Sorted: by `uid` descending, so the most recent upload is at
the top. Sort key is stable across polls.

| Column | Meaning |
|---|---|
| UID | Bee's per-tag id |
| LABEL | Operator-supplied label (or `—` if none) |
| ADDRESS | Short reference for the upload root |
| STATUS | Five-state lifecycle (see below) |
| % | `synced / total × 100`, clamped to 0–100 |
| SYNCED / TOTAL | The raw counters Bee reported |

The summary header above the table shows the rollup:

```
TAGS   total 4   active 2   synced chunks 12,860 / 13,641
```

`active` = tags currently in Splitting / Pushing / Syncing —
"work in flight".

## The status ladder

| Status | Glyph | When |
|---|---|---|
| Pending | · pending | `total <= 0` — Bee hasn't filled the chunk count yet (upload either hasn't started or used a streaming endpoint that doesn't pre-declare) |
| Splitting | ▒ splitting | `split < total` — chunker is still slicing the input |
| Pushing | ▒ pushing | All chunks split, pushing them out: `sent < total` |
| Syncing | ▒ syncing | All pushed but waiting on receipts: `synced < total` |
| Synced | ✓ synced | `synced >= total > 0` — the upload is done |

The three "in flight" states (Splitting / Pushing / Syncing)
are coloured warn-yellow so they all read as "working,
don't unplug yet". Pending is dimmed (cold). Synced is green.

### Why `seen` matters but doesn't have a stage

`seen` counts chunks the network already had — Bee skips
re-pushing them. A tag with high `seen` finishes faster
because there's less network traffic. But it doesn't change
the *lifecycle* — the tag still goes Splitting → Pushing →
Syncing → Synced; it just spends less time in Pushing.

If you upload duplicate content (e.g. the same large file
twice), the second tag's `seen` will be near `total` and
the upload will complete almost instantly. This is normal.

## Common scenarios

### "Tag stuck at 99 % synced"

Bee waits for `synced == total` exactly. The last 1 % can
take longer than the first 99 % because:

- Some chunks are at deep replication depth where peers are
  sparse
- A handful of receipts haven't come back yet (network jitter)
- Your chequebook ran low during push and Bee paused

Wait 5 min. If still stuck, check S3 Swap for chequebook
balance and S6 Peers for bin saturation.

### "All my tags are Pending forever"

You used a streaming upload endpoint that doesn't pre-declare
total. Tags from `POST /chunks/stream` (websocket) and
similar may stay Pending. The upload still works; the tag
just doesn't track progress meaningfully. Check the
underlying upload's reference instead.

### "Sent > Synced for a long time"

Normal. Sent means Bee pushed the chunk out; Synced means a
peer responded with a storage receipt. The gap is the
in-flight queue. If the gap is widening, push throughput is
exceeding sync throughput — common on slow networks. Will
catch up once you stop uploading.

### "tags screen shows hundreds of old tags"

Bee keeps tags forever unless you delete them. If you've
done a lot of uploads, the list grows. Use `:tag-prune` (when
implemented) or `DELETE /tags/<uid>` directly to clean up.

### "Address column is empty"

The tag was created but the upload finished or errored before
producing a root reference. Probably a failed upload. Safe to
ignore.

## Snapshot cadence

S9 polls `/tags` every 5 s — fast enough for upload progress
to feel live, slow enough not to hammer Bee while it's busy
pushing chunks. The call is cheap (no per-tag fan-out, just
the list).

## Keys

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Scroll the table by one row |
| `PgUp` / `PgDn` | Page through tags |
| `Home` | Jump to top |
| `?` | Toggle help overlay |

No selection cursor, no drill (yet) — the data per tag is
small enough to fit in one row. A future drill could expose
per-stage timing graphs but isn't in 1.0.
