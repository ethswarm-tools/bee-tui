# S8 — RPC / API health

Three panes covering Bee's local API performance, Bee's view
of the chain, and pending operator transactions. The screen
that answers "is the local Bee API responsive?" — separately
from "is the chain healthy?".

## Why this screen exists

The original PLAN was Gnosis-RPC latency + remote chain tip.
Bee doesn't expose its eth-RPC URL (intentionally — it's a
private endpoint), and there's no remote chain-tip reference
in the API. So S8 pivots to what we *can* measure:

1. **Bee API call stats** — latency p50 / p99 + error rate
   from the live `tracing` capture. This is the more
   operator-relevant metric anyway; a slow Bee API tells
   you the local node is sluggish, regardless of what the
   underlying RPC is doing.
2. **Chain state** — `block` / `chain_tip` / their delta
   from `/chainstate`. Bee's own view.
3. **Pending operator transactions** — `/transactions` with
   hash, nonce, creation timestamp, description.

The "Bee doesn't expose its eth RPC URL or remote block
height" gap is acknowledged inline, so operators see what
*isn't* being measured rather than assuming silence equals
success.

## Header

```
RPC / API HEALTH    Bee endpoint http://localhost:1633
  Bee doesn't expose its eth RPC URL or remote chain tip;
  this view measures the local Bee API instead.
```

The endpoint URL is the configured Bee URL — same one shown
in the top status bar. The disclaimer is fixed; it's not a
warning, just a clarification that we measure what we can.

## Pane 1 — Bee API call stats

```
  CALLS (last 100)
    p50 latency      45ms
    p99 latency     180ms
    error rate       0.0%
    sample size     100
```

Computed from the most recent 100 entries in the live
`LogCapture` ring buffer (the same buffer that powers S10's
command tail). Per-entry data:

- `elapsed_ms` — captured by the bee-rs HTTP client tracing
- `status` — HTTP response code

| Stat | How |
|---|---|
| p50 / p99 | Sort the window by elapsed, pick the median + 99th index |
| Error rate | `(count where status >= 400) / sample_size × 100` |
| Sample size | Number of entries with `elapsed_ms` set |

Window is fixed at the last 100 entries (`STATS_WINDOW`),
which matches the LogCapture ring buffer capacity (200) at
2× headroom. Lifting the cap above 200 wouldn't yield more
data — older entries are gone.

### Reading the stats

- **p50 < 100ms, p99 < 500ms, error rate 0 %** — healthy.
- **p99 climbing past 1 s** — Bee is under load. Could be
  upload + heavy reserve activity + a slow disk all at once.
- **Error rate > 1 %** — something's failing repeatedly. Tab
  to S10 Command log to see the actual error responses (most
  often 503 during warmup, or 401 if the auth token expired).

## Pane 2 — Chain state

```
  CHAIN
    block         234,512        from /chainstate
    chain tip     234,514
    delta         +2 blocks      ✓ in sync
    total amount  150000000000000000
    current price 24000
```

Bee's view of the chain. `delta = chain_tip - block`. Small
deltas (Δ +1 to Δ +3) flicker constantly — they're the
normal indexing lag between Bee processing a block and the
chain producing the next one. Sustained Δ ≥ +5 means Bee's
RPC is slow or dropping responses.

| Field | Meaning |
|---|---|
| `block` | Last block Bee has processed. |
| `chain tip` | What Bee's RPC reports as the head. |
| `delta` | Difference. Positive = Bee is behind. |
| `total amount` | Sum of all postage stamps issued (BZZ in PLUR). |
| `current price` | Bee's per-chunk-per-block stamp price (PLUR). |

`total_amount` and `current_price` are the on-chain stamp
parameters. They drive `batch_ttl`, so when the price moves,
every batch's TTL moves with it. This is also surfaced on S2.

## Pane 3 — Pending transactions

```
  PENDING TRANSACTIONS  (2)

  NONCE   HASH         TO            CREATED                DESCRIPTION
  47      0xabcd…ef    0x123…45      2026-05-07T08:12:03Z   stamp topup
  48      0x9876…12    0x123…45      2026-05-07T08:14:15Z   stake deposit
```

Operator-issued transactions that Bee has submitted but the
chain hasn't confirmed yet. Sourced from `/transactions`.

| Column | Meaning |
|---|---|
| NONCE | Operator wallet nonce |
| HASH | First 6 + last 4 hex of the tx hash |
| TO | First 4 + last 4 hex of the destination address |
| CREATED | RFC 3339 timestamp from Bee, rendered verbatim |
| DESCRIPTION | Operator-supplied description (empty for system txs) |

If a transaction has been pending for > 5 min, gas was
probably too low. You can re-broadcast or replace via
`POST /transactions/<hash>` (cancel/resend). The cockpit
doesn't do this for you — there's no in-cockpit cashout —
but the data is here so you know where to look.

## Common scenarios

### "p99 spiked to 5 s"

Bee is overloaded. Likely culprits:

- A big `/stamps/<id>/buckets` drill on a deep batch (just
  finished — the spike will fade).
- A reserve worker tick (every 30 min, lots of disk I/O).
- An upload that triggered chunk-pushing (S9 will show this).

If the spike doesn't fade in 5 min, check `iotop` on the
host — slow disks are the usual culprit.

### "Delta stuck at +5"

Bee's RPC is slow. Bee can't do anything about it; this is
the upstream Gnosis RPC. Switch RPC providers if the issue
persists. (Bee config is `--blockchain-rpc-endpoint`.)

### "Pending transaction sitting at 10+ min"

Check the gas price. If the transaction was submitted with a
gas price below the current chain floor, it'll sit in the
mempool until it's evicted. Use a chain explorer to inspect
the actual gas params — Bee's `/transactions` doesn't include
them in detail.

### "Total amount goes up but no batch I bought"

`total_amount` is the *network-wide* total, not yours. It
goes up whenever anyone on the network buys postage. Use
S2 Stamps for your local batches.

## Snapshot cadence

- `/chainstate` — 2 s (existing health stream)
- `/transactions` — 5 s (cheap call, low-rate change)

The call stats are recomputed every Tick (60 fps tick budget,
but the LogCapture itself only updates on actual HTTP events).

## Keys

S8 has no screen-specific keys. The global keymap (`Tab`,
`?`, `:`, `q`) covers everything.
