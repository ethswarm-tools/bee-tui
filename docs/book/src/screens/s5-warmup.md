# S5 — Warmup checklist

Five-step ladder showing where a new node is in the warmup
process. Each step has a clear "done" criterion and a detail
string that surfaces the *current value* against the target —
so operators don't just know "Reserve fill is in progress"
but "12,345 / 65,536 chunks (19 %)".

## Why this screen exists

A fresh Bee node won't earn rewards for the first ~30 minutes.
That's normal: the lottery only includes nodes that pass
`is_warming_up = false` plus a handful of internal checks
(reserve filled to depth, kademlia depth stable, sample worker
healthy). But Bee returns one boolean — `is_warming_up` — and
the operator has no way to see *which* of the underlying
preconditions is the holdup.

S5 unrolls the boolean. Five steps, each with its own target,
elapsed-time tracking, and a one-line detail. If a node is
stuck at "Reserve fill 14 %" 25 minutes in, you know the
issue isn't lottery code — it's that chunks aren't arriving
fast enough to fill the reserve. (Slow disk, slow network,
low peer count, tiny radius — the rest of the cockpit will
tell you which.)

## The five steps

| # | Step | Target | Source |
|---|---|---|---|
| 1 | Postage snapshot loaded | `/stamps` returned ≥ 1 batch | `StampsSnapshot.batches` |
| 2 | Peer bootstrap | `connected_peers ≥ PEER_BOOTSTRAP_TARGET` | `Status.connected_peers` |
| 3 | Kademlia depth stable | Depth unchanged across the observation window | `Topology.depth` + internal stability tracker |
| 4 | Reserve fill | `reserve_size_within_radius ≥ 65,536` | `Status.reserve_size_within_radius` |
| 5 | Stabilization | `is_warming_up = false` | `Status.is_warming_up` |

The order is roughly chronological — postage loads almost
immediately, peer bootstrap takes seconds, depth settles in
~1–2 minutes, reserve fill is the slow step (10–30 min on a
healthy mainnet node), and the final stabilization flag flips
shortly after reserve hits target.

## Step state ladder

Each row carries one of four states:

| State | Glyph | Meaning |
|---|---|---|
| Done | ✓ | Step satisfied. Move on. |
| InProgress(N) | ▒ | Step is partway done; `N %` shows the current fraction toward target. |
| Pending | ⏳ | Step hasn't moved yet (e.g. reserve is still 0 chunks). |
| Unknown | · | The relevant snapshot hasn't loaded yet. Cold start. |

## Reading a row

```
  ✓  Postage snapshot loaded         3 batch(es)
  ▒  Peer bootstrap                  47 connected (target ≥ 64)              74%
  ▒  Kademlia depth stable           depth 8 (still settling)                50%
  ▒  Reserve fill                    12,345 / 65,536 in-radius chunks        19%
  ⏳  Stabilization                    Bee still reports is_warming_up=true
```

The detail column is the *value* — Bee's actual numbers, not
a paraphrase. You can compare run-to-run, screenshot it for
support threads, and not worry that the cockpit is hiding
information. The right edge has a percentage progress bar
where applicable.

The header line shows the elapsed wall-clock time since the
cockpit first observed `is_warming_up = true`:

```
WARMUP CHECKLIST   elapsed 14m 23s
```

That elapsed counter is captured at first observation and
frozen the moment warmup completes — so once the node finishes
warming, the checklist stays useful as a record of *how long*
warmup took, with all five rows green.

## Common scenarios

### "Reserve fill stuck at single-digit %"

The reserve only fills as peers push relevant chunks to your
node. If reserve is climbing slowly (or not at all):

- Drop to S6 Peers and check the bin saturation strip. If
  bins near your kademlia depth are red ("Starving"), you
  don't have enough peers near your address space to receive
  chunks.
- Check S1 Health gates 7 (Bin saturation) and 5 (Peers).
  Both should be green for reserve to fill at a normal rate.
- A skewed dataset on the network can also cause uneven fill.
  This usually self-corrects within an hour.

### "Peer bootstrap stuck at 12 / 64"

Either the node hasn't found bootnodes (check S7 Network for
a public address and `/addresses` connectivity), or it's NAT-
trapped. AutoNAT will report `Private` on S7. Operators behind
double-NAT typically stall here.

### "Kademlia depth bouncing 7 → 8 → 7"

Normal during the first 60–90 seconds. The "stability" detector
waits for the depth to hold for an observation window before
calling it stable. If it's bouncing for >10 minutes, check
S6 Peers — depth instability is usually peer churn.

### "Postage snapshot loaded says no batches"

Bee will warm up without postage, but you can't upload
anything until you buy a batch. Tab to S2 Stamps, run
`bee postage buy <amount> <depth>` from a separate shell,
wait ~10 blocks for confirmation, and the row will go green.

### "Stabilization says complete but Reserve says 47 %"

Bee's `is_warming_up` flag flips once the *minimum*
preconditions are satisfied — it doesn't actually require a
full reserve. Reserve will keep filling while the lottery is
already enabled. This means S4 Lottery's stake card may go
healthy before reserve is full; that's fine.

## Snapshot cadence

S5 piggy-backs on the streams S1 already runs — no extra HTTP
calls:

- `/status` (2 s) — warmup, peers, reserve
- `/topology` (5 s) — depth + stability tracking
- `/stamps` (5 s) — first batch detection

The depth-stability tracker is internal to the Warmup
component; it watches the topology snapshots and only flips
the step to Done after the value has held for the observation
window.

## Keys

S5 has no screen-specific keys. The global keymap (`Tab`,
`?`, `:`, `q`) covers everything.
