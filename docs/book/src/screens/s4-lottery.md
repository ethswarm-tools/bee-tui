# S4 — Lottery / redistribution

Three panes covering the storage incentives game (the
redistribution lottery): round timeline, anchor summary, and
a six-state stake card. Plus an on-demand rchash benchmark.

## Why this screen exists

Bee earns BZZ through the redistribution lottery — every 152
blocks, eligible nodes commit a hash of a sample of their
reserve, reveal it, and (if they win the round) claim the
reward. The mechanics span four scattered RedistributionState
booleans (`is_frozen`, `is_healthy`, `has_sufficient_funds`,
`is_fully_synced`), the staked amount, and the per-round
`LastWonRound` / `LastPlayedRound` / `LastSelectedRound` /
`LastFrozenRound` anchors.

When an operator asks "why am I not earning rewards?",
neither `/redistributionstate` nor `/stake` alone answers.
S4 reduces it all to a single screen with explicit reasoning
trees.

## Pane 1 — Round timeline

```
ROUND 4127   block 234,512  ·  in round 87/152
  commit  ████████████░░░░░░░░░░░░  blocks 1-38
  reveal  ████████████████████████  blocks 39-76
  claim   ████████████░░░░░░░░░░░░  blocks 77-114
  idle                              blocks 115-152
```

The 152-block round is split into three on-chain phases per
`pkg/storageincentives/agent.go`:

- **Commit** (blocks 1-38): submit a hash of your reserve sample
- **Reveal** (blocks 39-76): reveal the sample
- **Claim** (blocks 77-114): if won, claim the reward
- Idle (blocks 115-152): wait for next round

The progress bar shows where the current round is. Whether
*you* committed / revealed depends on your stake state +
RedistributionState booleans — see the stake card below.

## Pane 2 — Anchor summary

```
ANCHORS
  Last won            round 4115     12 rounds ago
  Last played         round 4126     this round
  Last selected       round 4126     this round
  Last frozen         round —        never
```

Four anchors with human Δ strings:

- **Last won**: the most recent round you claimed a reward
- **Last played**: the most recent round you committed a hash
- **Last selected**: the most recent round Bee said the
  network selected your sample (precondition for winning)
- **Last frozen**: the most recent round you were frozen out
  (penalty for misbehaviour)

The Δ string ("12 rounds ago", "never", "this round")
calibrates the cadence. A fresh node should be playing every
round once warm; if Last played is many rounds behind Last
selected, you're missing commits.

## Pane 3 — Stake card

The most operator-relevant pane. Six states, each with an
explicit reason:

| State | When | What to do |
|---|---|---|
| Healthy ✓ | Stake > 0, not frozen, healthy, sufficient funds | Nothing. You're playing rounds correctly. |
| Unstaked · | Stake = 0 | Run `bee stake deposit <amount>` to enter the lottery. |
| Frozen ✗ | `is_frozen = true` | Penalty round. Wait it out (variable duration; check Last frozen anchor for the round you got frozen). |
| InsufficientGas ⚠ | `has_sufficient_funds = false` | Native balance too low to play. Top up the operator wallet. |
| Unhealthy ⚠ | `is_healthy = false`, other booleans OK | Reserve isn't filled / depth not stable / fully synced still false. Most common during warmup; see S5. |
| Unknown · | Snapshot not loaded | Cold start. |

The reasoning tree fires the first match top-to-bottom, so
"InsufficientGas + Unhealthy" reads as `InsufficientGas`
(more actionable).

## Pane 4 — Rchash benchmark (on-demand)

Press `r` to fire `GET /rchash/<depth>/<anchor1>/<anchor2>`
where:

- `depth` = current `storage_radius`
- `anchor1`, `anchor2` = deterministic so repeat measurements
  compare cleanly

The result shows the duration vs the **95-second commit window
deadline**:

```
RCHASH BENCHMARK
  duration   3.4s
  hash       0xabcd…
  budget     ✓ well under 95s commit deadline
```

If duration approaches or exceeds 95 s, your reserve is too
slow to commit in time. The lottery will silently skip your
node every round. Possible causes:

- Reserve is on a slow disk (HDD, network-attached storage)
- Bee is competing with other I/O (database, video)
- Storage node has very high `committedDepth`

Lifecycle is owned by an internal mpsc inside the Lottery
component, not a global Action — so `r` doesn't pollute
other screens, and a benchmark already in flight is
no-op'd by re-pressing `r`.

## The "why am I not earning rewards?" decision tree

1. Stake card says **Unstaked** → deposit stake
2. Stake card says **InsufficientGas** → top up native balance
3. Stake card says **Frozen** → wait it out
4. Stake card says **Unhealthy** → check S1 for which gate is
   failing; if Reserve isn't filled, see S5 Warmup
5. Stake card says **Healthy** but Last won is many rounds
   behind Last played → press `r` and check the rchash
   duration; if it's near 95 s, your reserve is too slow
6. Healthy + good rchash + still not winning → the lottery
   is stochastic; some rounds you don't get selected. Watch
   Last selected vs Last won — if Selected is recent but
   Won is old, the network reveals didn't include your
   sample (rare but possible).

## Snapshot cadence

S4 polls two streams:

- `/redistributionstate` (existing 2 s health stream — shared)
- `/stake` (30 s, low-rate)

The rchash benchmark is on-demand only.

## Keys

| Key | Effect |
|---|---|
| `r` | Fire / re-fire rchash benchmark |
| `?` | Toggle help overlay |

No selection cursor (yet) — the screen is mostly cards, not
a list.
