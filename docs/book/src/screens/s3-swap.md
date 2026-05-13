# S3 — SWAP / cheques

Three stacked panes covering the chequebook (off-chain
accounting layer Bee uses to settle inter-peer payments) and
its on-chain counterpart, settlements.

## Why this screen exists

Bee's pricing protocol means every chunk forwarded between
peers gets paid for in BZZ. Most of that payment doesn't go
on-chain — peers exchange *cheques* off-chain and only cash
them in periodically. This means at any moment:

- Your **chequebook balance** holds total + available BZZ
- Peers have **received cheques** from you that haven't been
  cashed yet (uncashed debt)
- You've **received cheques** from peers, also uncashed
- Net per peer = received − sent

S3 surfaces all four numbers so operators can answer "do I
need to cash out?" and "is any one peer way out of balance?".

## Header

```
SWAP / CHEQUES   contract 0xCE3EE0201A1A8296E8bC2BE9f912eC21708fd615
```

The contract address is the on-chain chequebook — useful for
pasting into a block explorer. It's surfaced via bee-rs
1.5's `chequebook_address` endpoint and only shown once it's
fetched (silently absent during the first second after
launch).

## Pane 1 — Chequebook card

```
  Chequebook  ✓  available BZZ 8.0000  /  total BZZ 10.0000  (80%)
```

Three states for the card:

| Status | When | What it means |
|---|---|---|
| Healthy ✓ | available / total ≥ 50 % | Plenty of headroom. Operations work. |
| Tight ⚠ | available / total < 50 % | Uncashed debt is eating into headroom. Cashing out may be wise — see Pane 2. |
| Empty ✗ | total = 0 | Chequebook hasn't been funded. Cheque-based settlement is unavailable; only time-based pseudo-settlement works. |
| Unknown · | snapshot not loaded | Cold start — wait. |

The percentage in parens is `available / total` rounded.

## Pane 2 — Last received cheques

```
  PEER          PAYOUT          ISSUED
▸ cccccc…cccc   BZZ 1.5000      8412930
    peer 0xcccccc8e2f1a40d7a0bf6e1c0a8a2c91e3b…
  bbbbbb…bbbb   BZZ 0.7500      8412901
  aaaaaa…aaaa   never           —
```

Sort: payout descending, with peers that have never sent us
a cheque (`never`) sinking to the bottom. Absence is signal
too — peers we've never been paid by are visible so the
operator can see the split.

If you want to cash out, this is the table to look at. The
`PAYOUT` column is the cumulative sent-to-us amount; cashing
moves it from off-chain to on-chain.

The cursored row (`▸`) prints a `peer 0x<full>` continuation
line so the full peer address is reachable for copy without
scrolling away (added in v1.9.1 — early versions only showed
the truncated `cccccc…cccc` form, which was insufficient when
you actually needed to paste it into a block explorer).

## Pane 3 — Per-peer settlements

```
  PEER          RECV         SENT         NET
▸ bbbbbb…bbbb   BZZ 8.0000   BZZ 1.5000   +6.5000
    peer 0xbbbbbb4c9e7a31f5d2c08e914a72bef0a3b…
  cccccc…cccc   BZZ 0.4000   BZZ 0.9000   -0.5000
  ddddd…dddd   BZZ 2.1000   BZZ 1.9000   +0.2000  ⚠
```

Sort: `|net|` descending so the most out-of-balance peer is
at the top. A `⚠` flag marks rows where `|net| > 0.5 BZZ` —
that's where cashout pressure builds up first. The cursored
row gets the same `peer 0x<full>` continuation treatment as
Pane 2 (v1.9.1).

The `+` / `-` signs on net read at a glance:

- `+` = peer owes us (we forwarded their chunks; they paid via cheque)
- `-` = we owe peer (they forwarded our chunks; we paid via cheque)

A persistent positive net with one peer and a high payout in
Pane 2 = cash that cheque. A persistent negative net = you're
sending more chunks than you're storing for them; might mean
your chequebook funding is the bottleneck on uploads.

## Time-based settlements

Bee 2.7+ also does *time-based pseudo-settlement* (refresh-rate
based, not cheque-based). The header line shows the totals:

```
  time-settlements   total received BZZ 12.5  ·  total sent BZZ 11.2
```

These don't show up per-peer in Pane 3 — they're aggregated at
the top of the snapshot.

## Market tile (v1.4.0+, opt-in)

Setting `[economics].enable_market_tile = true` in `config.toml`
appends a fourth tile to the screen with cost-context numbers
the chequebook itself doesn't carry:

```
  Market   xBZZ ≈ $0.4321   ·   gas 12.3 base + 1.0 tip = 13.3 gwei
```

- The **xBZZ price** comes from a public token service (no key,
  no auth). Cached for 60 seconds.
- The **basefee** and **tip** read the configured Gnosis JSON-RPC
  endpoint (`[economics].gnosis_rpc_url`, required for the gas
  half of the tile). Same 60 s cadence.

The tile is always visible when enabled — no `Unknown` ladder —
because the source feeds are external and a transient miss
shouldn't blank the screen. Stale numbers render in `dim`; fresh
numbers in `info`. The two underlying verbs `:price` and
`:basefee` print the same numbers on demand, useful for a quick
glance without flipping a config knob.

## Common scenarios

### "Tight chequebook"

Look at Pane 2's top peer. If their PAYOUT is > 0, you've
already received cheques from them — cashing those out moves
the BZZ from "uncashed debt" to "available chequebook
balance". The cashout is on-chain (gas costs), so don't do
it for tiny amounts.

### "All my settlements are negative"

You're forwarding more chunks than you're storing, and paying
peers via cheques to do so. This is normal for low-radius
nodes (you're closer to roots of the kademlia tree than to
leaves). If it's bothering you, increase your radius / depth.

### "One peer is way out of balance, +5 BZZ"

That peer has been paying you reliably. Look at their cheque
in Pane 2 — if it's a single big payout, it's a normal
infrequent-but-bulk pattern. If it's many small ones, they're
a high-volume forward partner.

### "Total received BZZ is huge but available is tiny"

Most of the received BZZ is uncashed cheques sitting in Pane 2.
Cash some out (see the next page on commands — there's no
in-cockpit cashout, but you can curl `POST /chequebook/cashout/<peer>`).

## Snapshot cadence

S3 polls four endpoints at 30 s — chequebook + settlement state
changes at chain rate, no point hammering:

- `/chequebook/balance`
- `/chequebook/cheque` (last received per peer)
- `/settlements`
- `/timesettlements`
- `/chequebook/address` (once-ish — header data)

## Batch economics modal (v1.12.0+)

Press `E` (Shift+E) anywhere in the cockpit to open the
batch-economics modal — a guided form that runs the same
preview verbs (`:topup-preview` / `:dilute-preview` /
`:extend-preview` / `:buy-preview` / `:plan-batch`) without
making you remember argument order.

```
┌─ batch economics ─────────────────────────────────────────┐
│   Pick an action:                                         │
│                                                           │
│   [t] topup-preview   predict TTL gain from topping up... │
│   [d] dilute-preview  predict utilisation drop from depth │
│   [e] extend-preview  what would N more seconds of TTL... │
│   [b] buy-preview     predict TTL of a fresh batch at...  │
│   [p] plan-batch      unified topup + dilute recommend... │
│                                                           │
│   Press the letter to choose · Esc cancels                │
└───────────────────────────────────────────────────────────┘
```

Type the action letter, then fill the fields one at a time
(`Enter` commits each field, `Esc` cancels at any point).
After the last field, the preview output appears inline.
`Enter` or `Esc` dismisses.

The result of each preview is identical to the typed verb's
status-line output — the modal builds the verb line under
the hood and runs the same `run_*_preview` method. So you
can use either path interchangeably depending on whether
you remember the args.

## Keys

S3 has no screen-specific keys (no drill yet — peer drill
on this screen would duplicate S6). Use S6's peer drill if
you want per-peer cheque + settlement detail with ping RTT
included.
