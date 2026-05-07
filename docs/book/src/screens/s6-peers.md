# S6 — Peers + bin saturation + drill

The screen most operators end up living on. A 32-row bin
strip showing kademlia health at a glance, a peer table
sorted by bin / latency, and a per-peer drill that fans out
four endpoints in parallel.

## Why this screen exists

Bee's `/topology` returns 32 bins of peer data, with per-bin
`connectedPeers`, `disconnectedPeers`, and a fair amount of
metric fields per peer (latency EWMA, session direction,
reachability). The relevant numbers are scattered across
4-deep JSON nesting and *most* of them are noise on any given
day — operators want three things:

1. **Are the bins near my depth saturated?** This determines
   whether the reserve is fillable and whether forwarding can
   work. See the saturation strip.
2. **Are individual peers healthy?** Latency, session
   direction, reachability — the peer table.
3. **What's *one specific* peer up to?** Balance, ping,
   settlements, cheques — the drill.

Bee's own `/topology` is too dense for any of these. S6 is
the cockpit's heaviest pre-render: it computes bin saturation,
sorts peers, and aggregates the four-way drill into one pane.

## Pane 1 — Bin saturation strip

```
BIN SATURATION   depth 8 · 142 connected (3 light)

  bin  pop   connected   status
  0    23    14           ✓
  1    18    11           ✓
  2    14     9           ✓
  ...
  7    11     7           ✗ STARVING        ← below depth, only 7 peers
  8    14    11           ✓                 ← at depth, healthy
  9     5     3           —                  ← far from depth, naturally sparse
  ...
```

The strip is one row per bin, 0..31. For each row:

- `pop` = total population (connected + disconnected)
- `connected` = currently connected
- `status` = the four-state classification below

### Saturation classification

The thresholds are pulled directly from bee-go's
`pkg/topology/kademlia/kademlia.go`:

| Status | Glyph | When |
|---|---|---|
| Healthy | ✓ | `connected ∈ [8, 18]` |
| Starving | ✗ STARVING | `connected < 8` AND the bin is *relevant* (≤ depth) |
| Over | ⚠ over | `connected > 18` (Bee will trim oldest entries; harmless) |
| Empty | — | `connected == 0` AND the bin is *not* relevant (far from depth) |

A bin is *relevant* if `bin ≤ depth + FAR_BIN_RELAXATION`
(currently 4). Far bins with low population are normal — the
network is simply sparse out there — so we don't flag them.

The headline question this strip answers: *do my relevant
bins have ≥ 8 peers each?* If yes, your kademlia health is
fine and reserve fills will work. If no, you're starving.

## Pane 2 — Peer table

```
  PEER          BIN   DIR   LATENCY   HEALTH   REACHABILITY
▶ aaaa…aaaa     8     in    12ms      ✓        Public
  bbbb…bbbb     8     out   45ms      ✓        Private
  cccc…cccc     7     in    8ms       ⚠        Public
  dddd…dddd    14     in    23ms      ✓        Public
```

Sort: by bin ascending, then by latency ascending within a
bin. The `▶` cursor marks the row that `↵` will drill into.

| Column | Meaning |
|---|---|
| PEER | Short overlay address (first 4 + last 4 hex) |
| BIN | Kademlia bin (0–31) |
| DIR | `in` (we accepted their dial) / `out` (we dialed them) / `?` (no metric yet) |
| LATENCY | Bee's EWMA latency value, formatted as `Xms`. `—` if not yet measured. |
| HEALTH | `✓` healthy, `⚠` un-healthy from per-peer metrics |
| REACHABILITY | Bee's per-peer AutoNAT string (Public / Private / empty) |

The table is scrollable: `j/k`/`↑↓`/`PgUp`/`PgDn`/`Home` move
the cursor and the body scrolls under a pinned header. A
right-edge scrollbar shows your position.

## Pane 3 — Peer drill (Enter on a row)

`↵` fires four endpoints in parallel for the selected peer:

- `GET /peers/<overlay>/balance` → settlement balance
- `GET /pingpong/<overlay>` → live RTT
- `GET /settlements/<overlay>` → received + sent BZZ
- `GET /chequebook/cheque/<overlay>` → last cheques

```
PEER  aaaa…aaaa   bin 8

  Balance              +0.0042 BZZ           (peer owes us)
  Ping (live)          5.0018ms
  Settlement received  BZZ 2.4500
  Settlement sent      BZZ 1.7800
  Last received cheque BZZ 1.5000
  Last sent cheque     —
```

Each row is rendered independently — if `pingpong` 404s
(peer disconnected mid-fetch) but the other three succeed,
the drill still shows three rows + an inline error on Ping.
Partial failure is the rule, not the exception, when peers
churn.

The four fetches use `tokio::join!` so the drill window
opens within ~2× the slowest endpoint, not 4× the average.

`Esc` closes the drill and restores the peer table.

## The bin saturation thresholds — bee-go constants

The numbers `8` and `18` aren't cockpit decisions. They're
hardcoded in bee-go:

```go
// pkg/topology/kademlia/kademlia.go
const SaturationPeers     = 8
const OverSaturationPeers = 18
```

S6 mirrors them so the cockpit's "Starving" verdict is the
*same* verdict Bee makes internally when deciding whether to
keep dialing peers in a bin. If the strip says Starving, Bee
itself is also unsatisfied with that bin and will keep dialing
when given the chance.

## Common scenarios

### "Bin 8 is starving but bin 0 has 30 peers"

Normal for a new node. Bins close to bin 0 (peers furthest
from your address) saturate first because there are simply
more of them. Bins near your depth (where chunks live) take
longer because the global address density is lower out there.
Wait. If it's been 30+ minutes and depth bins still have <4
peers each, your node may not be reaching the bootnode set —
check S7 Network.

### "Multiple bins say Starving below depth"

Reserve fill will be slow or stuck. Check the connectivity
basics first:

- S1 Health gate 5 (Peers) — total connected count
- S7 Network — reachability + advertised underlays
- `/connect` endpoint (via curl) — manually dial a known good
  bootnode

If you're behind NAT (S7 says Private), expect bin starvation
on inbound bins unless you set up port forwarding or a relay.

### "Drill shows ping 200ms+"

That peer is genuinely far away or congested. Bee will route
around them; they'll get cycled out as Bee's EWMA latency
favours faster peers. No action needed.

### "Drill 'last received cheque' shows BZZ 5+"

You haven't cashed it. That much uncashed value with one peer
is unusual — either they're a major forwarding partner (good)
or your chequebook hasn't been topped up enough to settle
on-chain (action: check S3 Swap).

### "Per-peer reachability is empty for everyone"

Older Bee builds didn't populate per-peer reachability. The
cockpit shows a blank column rather than guess. Upgrade Bee
or just rely on the global reachability on S7.

## Snapshot cadence

S6 piggy-backs entirely on the shared `/topology` stream
(5 s cadence). No dedicated S6 polling. The drill fires four
endpoints on demand and is *not* refreshed automatically —
close + re-open to get a fresh fan-out.

## Keys

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move cursor in peer table |
| `PgUp` / `PgDn` | Page through peers |
| `Home` | Jump to first peer |
| `↵` | Drill into selected peer (4 endpoints in parallel) |
| `Esc` | Close drill |
| `?` | Toggle help overlay |
