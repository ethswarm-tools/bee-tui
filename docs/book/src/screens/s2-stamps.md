# S2 — Stamps + bucket drill

Postage batch table with the volume + duration framing the
Bee community is moving toward (bee#4992 is retiring depth
+ amount), plus a per-batch drill that surfaces *which*
bucket is about to overflow.

## Why this screen exists

Bee's `/stamps` endpoint exposes a `utilization` field that
operators routinely misread. It's documented in OpenAPI as
"the average usage of the batch" — but the implementation
stores `MaxBucketCount`: the *peak* fill across all 2^bucket_depth
buckets. A batch with 1024 buckets at 0 chunks each and one
bucket at 64 chunks reads `utilization = 64`, not `0.06`.

Operators see "utilization 14 %" and think they have headroom.
Then their next upload fails with `ErrBucketFull` because the
worst bucket is actually at 95 %.

S2 puts the worst-bucket fill bar front and centre. The drill
goes deeper: it shows the full distribution, so two batches
with the same headline `utilization` reveal whether the load
is concentrated in one bucket or spread across many.

## The list view

```
 LABEL                BATCH        VOLUME      WORST BUCKET                TTL         STATUS
 prod-mainnet         abc123de…    16.0 GiB    ▇▇▇▇▇▇░░  78% (50/64)       47d 12h     I ✓
 spillover            def456ab…    16.0 GiB    ▇▇▇▇▇▇▇▇  98% (63/64)       12d  3h     I ⚠ skewed
        └─ worst bucket 98% > safe headroom — dilute or stop using.
 fresh-buy            789bc123…    16.0 GiB    ░░░░░░░░   0% (0/64)         1d  0h     I ⏳ pending
        └─ waiting on chain confirmation (~10 blocks).
```

| Column | Meaning |
|---|---|
| Cursor | `▶` marks the row Enter would drill into |
| LABEL | Operator-set label, or `(unlabeled)` |
| BATCH | First 8 hex chars of the batch ID |
| VOLUME | Theoretical capacity = `2^depth × 4 KiB` |
| WORST BUCKET | Fill bar + percentage + `utilization / BucketUpperBound` raw count |
| TTL | Days + hours remaining at current paid balance |
| I/M | `I` = immutable, `M` = mutable |
| STATUS | Five-state ladder (see below) |

## The status ladder

| Status | Glyph | When |
|---|---|---|
| Pending | ⏳ | `usable = false` — chain hasn't confirmed the batch yet (~10 blocks). |
| Healthy | ✓ | Worst bucket < 80 %, batch usable, TTL > 0. |
| Skewed | ⚠ | Worst bucket ≥ 80 % — above the safe headroom line. Dilute or stop using. |
| Critical | ✗ | Worst bucket ≥ 95 %. The very next upload may fail. |
| Expired | ✗ | `batch_ttl ≤ 0` — paid balance exhausted. Topup or stop using. |

## Immutable vs mutable — bee#5334

The `I/M` column matters more than it looks. **Immutable
batches** reject upload when a bucket overflows
(`ErrBucketFull` from Bee). **Mutable batches** silently
overwrite the oldest chunks in the full bucket. The Critical
tooltip splits accordingly:

- Immutable: `"immutable batch will REJECT next upload at this bucket."`
- Mutable: `"mutable batch will silently overwrite oldest chunks."`

If you're using mutable batches and the cockpit shows Critical,
your data is *probably* still on the network — but newer
uploads to that bucket are dropping older ones. There's no
warning from Bee.

## The drill (Enter on a row)

`↵` fires `GET /stamps/<id>/buckets` and renders the result
as a histogram + worst-N table:

```
  depth 22   bucket-depth 16   per-bucket cap 64   65,536 buckets
  total chunks 421 / 4,194,304   worst bucket 98%

  FILL %       COUNT   DISTRIBUTION
  0 %          65,400  ▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇▇
  1 – 19 %         88  ▇▇▇▇▇
  20 – 49 %        24  ▇▇
  50 – 79 %        12  ▇
  80 – 99 %         8
  100 %             4

  WORST BUCKETS
  #3         64 / 64    100%
  #17        63 / 64    98%
  #101       60 / 64    93%
  ...
```

### Reading the histogram

The six bins are sorted least-to-most full. The bar widths
are scaled to the *largest* bin, so the operator's eye locks
onto the densest range. Bin colours follow the fill:

- Pass (green): 0–79 %
- Warn (yellow): 80–99 %
- Fail (red): 100 %

If your batch is failing uploads, the red bin (`100%`) tells
you exactly how many buckets are saturated. If that count is
small (1-4), the load is concentrated and a `:dilute` would
help — diluting halves every bucket count by spreading chunks
across twice as many buckets. If it's large (50+), the batch
is genuinely full and no dilute will save it; cut a new batch.

### The worst-N table

Up to 10 entries, sorted by collisions descending, ties broken
by bucket-id ascending (stable across polls). Zero-count
buckets are filtered out. If your batch has fewer than 10
non-zero buckets, the table shows whatever's there.

The bucket IDs themselves are deterministic — bucket `i`
holds chunks whose first `bucket_depth` bits hash to `i`. This
isn't actionable for the operator (you can't choose which
bucket a chunk lands in), but knowing it explains *why*
saturation is uneven: bucket selection is hash-driven, not
load-balanced.

## Common scenarios

### "Worst bucket 95 % but I haven't uploaded much"

You probably uploaded a structured dataset — say, a directory
of files with similar names. Mantaray packs related entries
into the same chunks; if their hashes happen to share the
same `bucket_depth` prefix, they all hit the same bucket.
The drill will show one or two saturated buckets with the
rest near-empty. Solution: dilute the batch, or for very
skewed cases, cut a new batch and restart the upload.

### "All buckets are around 60 %, batch reads 60 % utilization"

You've been uploading random / well-distributed data. The
batch is genuinely 60 % full. Watch the worst-bucket value;
once it crosses 80 %, plan a dilute or topup.

### "Pending for more than 10 minutes"

Batches confirm after Bee sees the batch-create transaction
land on chain. If the operator wallet has insufficient gas,
the transaction stays in the mempool. Tab to S8 API → pending
transactions; if the buy is there with `pending > 5min`, top
up native balance.

### "TTL is dropping faster than expected"

`batch_ttl` is a function of `paid_balance / current_price`.
If Bee's `current_price` (from `/chainstate`) goes up, every
existing batch's TTL drops proportionally. This is normal
network repricing — you didn't lose money, the batch's
remaining lifetime just got shorter. Topup if you need it
to last longer.

## Keys

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move row selection |
| `↵` | Drill into selected batch |
| `Esc` | Close drill |
| `?` | Toggle help overlay |

## Snapshot cadence

S2 polls `/stamps` every 5 s — slow-changing data (TTL
drifts at chain rate, utilization grows at upload rate).
The drill fires `/stamps/<id>/buckets` on demand and is
not refreshed automatically — close + re-open the drill
to refresh, or wait for the next list-view tick.
