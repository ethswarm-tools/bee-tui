# S1 — Health gates

The first screen, default view on launch. Eleven gates with a
tri-state status ladder (Pass / Warn / Fail / Unknown), each
carrying a tooltip that encodes tribal knowledge about *why*
a gate fails the way it does.

## Why this screen exists

Bee returns plenty of data through `/health`, `/status`,
`/wallet`, `/redistributionstate`, and a handful of other
endpoints. The problem is calibration: a value of
`storageRadius = 7` on a node with `committedDepth = 8` looks
broken until you know that storageRadius decreases **only on
the 30-minute reserve worker tick** (bee#5428). Without that
context, operators stare at it for ten minutes wondering what
they did wrong.

S1 is the screen that hands you that calibration up front.

## The eleven gates

| # | Gate | What's checked | Source |
|---|---|---|---|
| 1 | API reachable | `/health` returns 200 within timeout | `HealthSnapshot.last_ping` |
| 2 | Chain RPC | Block tip vs chain tip from `/chainstate` (Δ ≤ a few blocks is healthy) | `ChainState.block` / `chain_tip` |
| 3 | Wallet funded | BZZ balance > 0 AND native balance > 0 from `/wallet` | `Wallet.bzz_balance` / `native_token_balance` |
| 4 | Warmup complete | `is_warming_up = false` from `/status` | `Status.is_warming_up` |
| 5 | Peers | Connected count from `/health` | `HealthSnapshot.connected_peers` |
| 6 | Reserve | `reserve_size_within_radius` vs `65,536` (Bee's reserve target at depth) | `RedistributionState.reserve_size_within_radius` |
| 7 | Bin saturation | Per-bin connected counts vs the bee-go `SaturationPeers=8` constant for relevant bins | `Topology.bins[].connected` |
| 8 | Healthy for redistribution | `is_healthy = true` from `/redistributionstate` | `RedistributionState.is_healthy` |
| 9 | Not frozen | `is_frozen = false` from `/redistributionstate` | `RedistributionState.is_frozen` |
| 10 | Sufficient funds to play | `has_sufficient_funds = true` from `/redistributionstate` | `RedistributionState.has_sufficient_funds` |
| 11 | Stamp TTL (v1.4.0+) | Worst-batch TTL across **usable** batches from `/stamps`. **Pass** when all usable batches have TTL > 7d; **Warn** when any drops under the 7d planning threshold; **Fail** when any drops under the 24h urgent threshold. Pending batches (`usable=false`) and nodes with zero usable batches show **Unknown** — operators on a fresh node would be surprised by a green stamp gate when no batches exist. | `StampsSnapshot.batches[].batch_ttl` |

## The status ladder

| Status | Glyph | Meaning |
|---|---|---|
| Pass | ✓ | Gate is satisfied. Move on. |
| Warn | ⚠ | Something off but not blocking — bin saturation flickering, chain RPC lagging by Δ +1 block. Keep an eye, no action required. |
| Fail | ✗ | Real problem requiring action. Read the tooltip on the next line. |
| Unknown | · | Snapshot hasn't loaded yet (cold start) OR the relevant endpoint returned no data. |

Status is rendered both as a glyph and a colour (green / yellow /
red / dim), so colourblind operators or `--ascii` users still
see the ladder via the glyphs.

## Reading a gate

Each gate occupies one line, plus an optional tooltip
continuation under it:

```
 ⚠  Bin saturation               2 starving: bin 4, bin 5
        └─ manually `connect` more peers or wait — kademlia fills bins gradually
```

The first column is the status glyph. The middle is the gate
label, padded to align. The right column is the *value* —
the specific number / string driving the status. The
continuation line (`└─` in the default theme) is the *why* —
a one-sentence explanation of what to do or what it means.

Tooltips only appear when there's something useful to say.
A green gate with `Pass` status doesn't need one.

## Common scenarios

### "Why is my Reserve gate failing?"

Look at the value. If it reads `12,345 chunks (in-radius: 12,345)
· radius 8`, your reserve is filling but hasn't reached the
65,536 chunk target Bee uses at depth. This is normal during
warmup — wait. The Warmup screen (S5) tracks this explicitly.

### "Bin saturation says Starving but I just connected to 12 peers"

The gate looks at *per-bin* counts, not total peer count. You
may have 100 connected peers all sitting in bin 0; the bins
near your kademlia depth (where chunks actually replicate)
might still have 3-4 peers each. Tab to S6 Peers and look at
the bin saturation strip — that's the canonical view.

### "Chain RPC shows Δ +5"

Your local Bee thinks the chain tip is 5 blocks ahead of the
last block it processed. Small lags (Δ +1, Δ +2) flicker
constantly and are normal. Sustained lag (Δ +5 for several
minutes) means your Gnosis RPC is slow or dropping responses.
Check the upstream RPC; Bee can't fix what RPC sends it.

### "Wallet funded is failing"

If BZZ is zero, you can't issue postage stamps and uploads
won't work. If native is zero, you can't pay gas — chequebook
operations and stake / redistribution will all stall. Top
up the operator wallet from a faucet (testnet) or your treasury
(mainnet).

### "Healthy for redistribution = Fail but Not frozen = Pass"

`is_healthy` looks at multiple internal preconditions
(reserve filled, depth stable, recent samples). A node can
be unfrozen but still un-healthy during the first
post-warmup window. Wait one or two redistribution rounds
(~5 minutes); if it stays un-healthy, drop down to S4 Lottery
which has a six-state stake card with the actual reasoning
tree.

## Snapshot cadence

S1 polls four endpoints at 2-second intervals:

- `/status` — warmup, peer count
- `/wallet` — BZZ + native balances
- `/chainstate` — block + chain tip
- `/redistributionstate` — frozen / healthy / funds

The 2 s cadence is fast enough that operator-visible state
changes feel live, slow enough not to hammer Bee. Per-bin
data for the saturation gate comes from the `/topology`
poller (5 s cadence) on the shared watch hub. The Stamp TTL
gate reads the S2 Stamps watch (15 s cadence) — TTL counts
down in seconds, so a slower poll is fine.

## Webhook alerts (v1.4.0+)

When `[alerts].webhook_url` is set in `config.toml`, bee-tui
diffs the gate states between ticks and POSTs a Slack /
Discord-compatible payload on every transition worth pinging
on (per-gate `Pass↔Fail` and `Pass↔Warn` flips, with `Unknown`
silenced so cold-start doesn't fire noise). Each alert carries
the gate label, the from / to status, and the why-tooltip — so
the receiving channel gets enough context to triage without
opening the cockpit.

A per-gate debounce window (`[alerts].debounce_secs`, default
`60`) suppresses thrash when a gate flickers around its
threshold. The top bar shows `alerts ●` whenever a webhook is
configured, so operators see at a glance whether outbound
pinging is on. See [`config.md`](../config.md#alerts) for the
full block.

## Keys

S1 has no screen-specific keys. The global keymap (`Tab`,
`?`, `:`, `q`) covers everything.
