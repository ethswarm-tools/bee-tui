# S15 — Fleet view

Multi-node health roll-up. One row per `[[nodes]]` entry in
`config.toml`, polled every 10 seconds in parallel against
`/health` + `/status` + `/stamps`, aggregated into a status
ladder operators can scan in two seconds — "is anything red?"

Shipped in v1.11.0.

## Why this screen exists

Through v1.10, bee-tui could switch between configured nodes
quickly (`Ctrl+N` picker, `:context` verb) but only *looked
at* one node at a time. Operators running 3-10 nodes had to
hop manually between them to confirm everything was healthy
— a stress test for memory and a friction tax on the
"morning check, nothing's on fire" workflow that should take
fifteen seconds.

S15 answers that one question across every configured node
without giving up the single-node depth-of-detail of S1-S14.
Press `Alt+5` to jump from any screen, scan the rows, and:

- All green: keep working
- One amber: drill in via Enter (which calls `switch_context`
  for that node and lands you on S1 Health to investigate)
- One red: same drill-in path; you're already in the right
  place to act

## What each row shows

```
FLEET  ·  4 configured  ·  3 pass  ·  1 warn

  NAME             ENDPOINT                                   STATUS    PEERS  WORST TTL     PING
▸ prod-eu ● ★      https://bee-eu.example.com                 pass        87        24d      12ms
  prod-us          https://bee-us.example.com                 warn        52        14h      48ms
      └─ stamp TTL under 7d — plan a topup
  staging          https://bee-stage.example.com              fail         —          —        —
      └─ unreachable (/health failed)
  local            http://localhost:1633                      pass        91         7d       2ms
```

| Column | What it shows |
|---|---|
| **NAME** | The `[[nodes]].name` field from `config.toml`. `●` marks the currently active context (the one S1-S14 are looking at). `★` marks the `default = true` entry. |
| **ENDPOINT** | `[[nodes]].url`. Truncated with `…` if longer than 42 chars. |
| **STATUS** | Aggregate roll-up: **pass** (all checks green), **warn** (at least one check in its warn band), **fail** (critical threshold tripped or unreachable), **…loading** (first probe hasn't returned). Colour-coded same as S1 gates. |
| **PEERS** | `connected_peers` from `/status`. `—` when the node is unreachable or hasn't returned yet. |
| **WORST TTL** | Lowest `batch_ttl` across this node's **usable** batches (immutable + mutable, `usable = true`). Human-readable unit (`d` / `h` / `m`). `—` when no usable batches. |
| **PING** | Round-trip on the `/health` probe in milliseconds. `—` when unreachable. |

The continuation line under non-pass rows shows the *why* —
operator-facing explanation. Same wording the S1 gates use.

## The status ladder

| Status | When | Drilling |
|---|---|---|
| **pass** | API reachable, not warming up, ≥ 4 peers, worst stamp TTL > 7 days | Nothing to do. |
| **warn** | One of: warming up · 1-3 peers · worst TTL ≤ 7 days | Enter → S1 Health to see which gate is amber. Often resolves on its own (warmup) or with a topup. |
| **fail** | One of: unreachable · auth fails · 0 peers · worst TTL ≤ 24 hours | Enter → S1 Health to act. A red fleet row deserves attention. |
| **…loading** | First probe still in flight | Wait 10 seconds. If it stays loading, the probe is timing out — same as `fail` minus the explicit why-line. |

The aggregate is **worst-of**: if a node is warming up *and*
has 0 peers, the row reads `fail` (the more-actionable
state). The why-line preserves the most critical observed
condition.

## Keys

| Key | Effect |
|---|---|
| `↑↓` / `j k` | Move row cursor |
| `Enter` | Switch context to the cursored node — calls the same `switch_context` as `Ctrl+N` / `:context`. The active-node `●` follows; daemons against the previous context are torn down (per v1.9.1). After switch you land on S1 Health for that node. |
| `r` | Re-poll the fleet right now (impatient operator escape hatch — doesn't wait for the 10 s tick) |

## Cadence + cost

The poller runs every **10 s**, fanning a 3-endpoint probe
out to each configured node in parallel via
`FuturesUnordered`. Per-probe **5 s timeout** — a slow or
unreachable node times out without blocking the others. The
arithmetic for a 6-node fleet:

```
6 nodes × 3 endpoints / 10 s = 1.8 reqs/s total, ~108/min
```

Modest. The 10 s cadence is also tuned to the rate at which
fleet-level state actually changes: stamp TTL counts down in
seconds but doesn't matter at second-resolution, and node
reachability flickering faster than 10 s would be a network
glitch worth ignoring anyway.

## What S15 deliberately does *not* do

- **No full watch hub per node.** Running N copies of the
  `BeeWatch` poller (S1 Health's source) would be 8× the
  request volume for marginal value. S15 fetches only what
  the fleet ladder needs.
- **No per-row drill pane.** To go deep into one node, press
  Enter to switch context and use the existing S1-S14.
  Splitting the per-node depth between two screens would
  duplicate logic for no operator benefit.
- **No subset filter.** Every `[[nodes]]` entry from
  `config.toml` is in the fleet. If you want a node out of
  the rotation, remove it from config. (A `[fleet].include`
  / `exclude` knob is the obvious extension if this bites,
  but it didn't in v1.11's design pass.)

## Configuration

S15 itself reads `config.nodes` directly — just maintain your
`[[nodes]]` list and they show up here automatically.

There's also an optional `[fleet]` section (v1.12+) that turns
on a **fleet-aggregate webhook**: instead of each node firing
its own `[alerts].webhook_url` ping during a network blip,
bee-tui consolidates per-node status transitions across the
whole fleet into a single rolled-up POST per coalesce window.
Set `[fleet].aggregate_webhook_url` to enable it; tune
`[fleet].aggregate_window_secs` (default 60). Per-node
`[alerts]` keeps working independently when both are set. See
[Configuration → `[fleet]`](../config.md#fleet--fleet-aggregate-webhook-v112-optional).
Fleet status transitions also feed the in-cockpit notification
center (v1.14+), the same as per-gate health transitions.

The `default = true` marker (used by `Ctrl+N` to pick the
landing node) doubles as the `★` indicator in the fleet
table. The active-context `●` follows whatever
`switch_context` (or the picker, or Enter on S15) set.

## Why it's an additive feature, not a config-mode

A pure-overview "fleet mode" (where the whole cockpit
showed multi-node summary and you couldn't get to
per-screen detail) would have been simpler to build, but
operators don't want either-or — they want both. S15
sitting alongside S1-S14, with Enter as the cheap bridge
back to single-node depth, gives you the dashboard view
without taking the X-ray vision away.
