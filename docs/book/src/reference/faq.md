# Operator FAQ

Questions that come up most in support threads, with the
shortest accurate answer for each. Each answer points to the
relevant screen + page for deeper context.

## Health & gates (S1)

### Why is my Reserve gate failing during warmup?

It's normal during the first 10–30 minutes. Reserve fills
to 65,536 chunks at depth, and chunks only arrive as peers
push them to you. See [S5 Warmup](../screens/s5-warmup.md);
the reserve-fill row tracks this explicitly.

If it's still failing 60+ minutes in, your bins are starving
(S6) or you're NAT-trapped (S7).

### What does Bin saturation = STARVING mean?

Some kademlia bin near your depth has fewer than 8 connected
peers — bee-go's hardcoded saturation threshold. Bee won't
forward / receive chunks well in that bin. See
[S6](../screens/s6-peers.md). Usually self-resolves within
30 min on a public node.

### Chain RPC shows Δ +5

Bee thinks the chain tip is 5 blocks ahead of what it's
processed. Small lags flicker; sustained Δ ≥ +5 means slow
RPC. Bee can't fix it — switch your `--blockchain-rpc-endpoint`.

### Wallet funded gate is failing

Either BZZ is 0 (can't issue stamps) or native is 0 (can't
pay gas). Top up the operator wallet. From a faucet on
testnet; from your treasury on mainnet.

## Stamps (S2)

### What does ⏳ pending mean on a stamp?

`usable = false` — the on-chain batch-buy transaction hasn't
been confirmed yet (~10 blocks on Gnosis ≈ 2 min). If it
sits pending > 10 min, your operator wallet is out of gas;
check S8 pending transactions. See
[S2](../screens/s2-stamps.md).

### My batch utilization says 14 % but uploads fail

Bee's `utilization` field is the **peak bucket count**, not
the average. A batch with 1023 empty buckets and one 95 %
bucket reads `utilization = 14 %` while the worst bucket is
about to overflow. The cockpit's "WORST BUCKET" column
shows the truth. Drill in (Enter) for the histogram.

### Should I use immutable or mutable batches?

Default to **immutable**. Mutable silently overwrites old
chunks when a bucket fills — you lose data without warning.
Immutable rejects the upload, which is annoying but obvious.
See [S2 § Immutable vs mutable](../screens/s2-stamps.md#immutable-vs-mutable--bee5334).

### TTL is dropping faster than expected

`batch_ttl = paid_balance / current_price`. When network
price goes up, every batch's remaining time shrinks
proportionally. You didn't lose money; the batch just got
shorter. Topup if needed.

## SWAP / cheques (S3)

### "Tight chequebook" — what now?

Cash out the largest received cheque (S3 Pane 2 top row).
That moves uncashed BZZ into your available chequebook
balance. There's no in-cockpit cashout — use
`POST /chequebook/cashout/<peer>` via curl. Don't cash tiny
amounts (gas eats them).

### All my settlements are negative

You're forwarding more chunks than you store. Normal for
low-radius nodes near the kademlia roots. Increase your
radius / depth if it bothers you. See [S3 § Common scenarios](../screens/s3-swap.md#common-scenarios).

### Total received BZZ is huge but available is tiny

Most of received is uncashed cheques sitting in S3 Pane 2.
Cash them out.

## Lottery (S4)

### Why am I not earning rewards?

Walk the decision tree on [S4](../screens/s4-lottery.md#the-why-am-i-not-earning-rewards-decision-tree).
Tl;dr:

1. Stake card says Unstaked → deposit stake
2. Stake card says InsufficientGas → top up native
3. Stake card says Frozen → wait it out
4. Stake card says Unhealthy → see S5 Warmup
5. Healthy + bad rchash → reserve too slow; check disk
6. Healthy + good rchash + still unlucky → it's stochastic;
   wait

### What's a normal rchash duration?

Below 10 s on a healthy node with SSD storage. If it's
approaching the **95-second commit deadline**, your node
will silently miss every round. Slow disk, network-attached
storage, or competing I/O are the usual causes.

### My stake card says Unhealthy but I'm not frozen

Bee's `is_healthy` checks multiple internal preconditions
(reserve, depth, samples). A node can be unfrozen but still
un-healthy during warmup or if reserve drops. Wait one or
two rounds; if persistent, drop to S5.

## Network (S7)

### Why is my reachability flickering Public ↔ Private?

Symmetric NAT. AutoNAT can't pin you down. The "stable for
Xm" counter on S7 will keep resetting. Either set up port
forwarding (TCP 1634), run on a public VPS, or accept that
you're outbound-only. See [S7](../screens/s7-network.md).

### I have 142 peers but Inbound shows 0

You're outbound-only — peers can't dial you back. Firewall
or NAT. Even with high outbound, chunks won't arrive
properly. Fix the firewall / NAT.

### What's a "Private" underlay?

An advertised multiaddr that's RFC 1918 (`10.*`,
`172.16.*`, `192.168.*`), link-local, or loopback. Bee
advertises everything it binds to; private addresses are
dimmed because peers outside your LAN can't dial them.

## Transactions (S8)

### How do I clear a stuck transaction?

The cockpit doesn't do it for you. From outside:

```sh
curl -X POST http://localhost:1633/transactions/<hash>/cancel
# or to bump gas + resend:
curl -X POST http://localhost:1633/transactions/<hash>
```

Add `-H "Authorization: Bearer <token>"` if your node has
auth. Check S8 again to confirm it's gone.

### p99 latency spiked to 5 s

Bee is busy. Reserve worker tick (every 30 min), large
upload pushing chunks, or slow disk. If it stays high, run
`iotop` and check disk health. See
[S8](../screens/s8-api.md).

## Tags / uploads (S9)

### Why is my tag stuck at 99 % synced?

Last 1 % is the slow tail — receipts haven't all come back.
Wait 5 min. If still stuck, check chequebook (S3) — Bee
pauses pushing when it can't pay forwarders.

### My tag says Pending forever

You used a streaming endpoint that doesn't pre-declare
chunk count. The upload still works; the tag just doesn't
track meaningfully. See [S9](../screens/s9-tags.md).

## Operations

### How do I switch between nodes?

Three ways:

- **`Ctrl+N`** (or `:nodes`, v1.10+) — opens a picker overlay
  listing every `[[nodes]]` entry; ↑/↓ to select, Enter to
  switch, Esc to cancel. The active row is marked `●` and the
  `default = true` row is marked `★`.
- **`:context <name>`** — typed switch (alias `:ctx`). Same flow
  under the hood.
- **S15 Fleet view** (v1.11+) — press `Alt+5` to open the
  fleet roll-up; cursor onto the node you want; Enter switches
  context to it (same `switch_context` flow as the other two)
  and lands you on S1 Health for that node.

Define the nodes in `config.toml`. See
[`:context`](../commands/context.md) and
[S15 Fleet view](../screens/s16-fleet.md).

### How do I monitor multiple Bees at once?

S15 Fleet view (v1.11+). It polls every configured `[[nodes]]`
entry every 10 seconds in parallel and shows one row per node:
aggregate status, peer count, worst stamp TTL, `/health` ping.
The roll-up is shallow on purpose — three endpoints per node so
the fan-out cost stays modest — and the row's Enter binding
switches context to that node for full per-screen detail when
you need it. Each node still fires its own `[alerts].webhook_url`
independently; the fleet view itself is a passive dashboard, not
an alert source.

### Can bee-tui start Bee for me, or only talk to a running one?

Both. By default `bee-tui` connects to whatever `[[nodes]]`
profile is active — that's the "talk to a running Bee" path
and it works against local or remote nodes. Set `[bee].bin`
and `[bee].config` in `config.toml` (or pass `--bee-bin` /
`--bee-config` on the CLI) and bee-tui will spawn that
binary, wait for its API to come up, then open the cockpit
on top. The wrapper sits over the connect path; it isn't a
separate mode.

### How do I turn on webhook alerts for unhealthy gates?

Set `[alerts].webhook_url` in `config.toml` to a Slack-compatible
incoming webhook URL. Optionally tune `[alerts].debounce_secs`
(default 60). bee-tui will POST on every gate transition worth
pinging on. The top bar shows `alerts ●` whenever it's
configured. See [`config.md`](../config.md#alerts) and
[S1 § Webhook alerts](../screens/s1-health.md#webhook-alerts-v140).

### My 5-node fleet flaps; I get 5 Slack pings per blip. Help?

Set `[fleet].aggregate_webhook_url` (v1.12+). bee-tui will
buffer status transitions across all nodes for the
`aggregate_window_secs` window (default 60s) and POST a single
consolidated message ("Fleet alert: 2 fail · 1 warn · …"). Per-node
`[alerts].webhook_url` keeps working independently — point both
knobs at the same channel for high-detail per-node alerts plus a
fleet-level digest, or keep them separate for "every-node →
operator-Slack, aggregated → ops-channel" patterns.

### Bee keeps crashing — can bee-tui restart it for me?

Yes (v1.12+). When bee-tui spawns Bee (`[bee].bin` set), add
`[bee.supervisor].auto_restart = true`. The watchdog re-spawns
on any exit with exponential backoff (1s → 2s → 4s → 30s cap)
and a sliding one-hour budget (default 6 restarts). The top-bar
chip now shows uptime + restart count: `bee running 4d3h (2 restarts)`,
turning red as `bee: max restarts (6/6) hit` when the budget is
exhausted. See [`config.md` § [bee.supervisor]](../config.md#beesupervisor--auto-restart-watchdog-v12-optional).

### How do I run `:durability-check` continuously?

`:watch-ref <ref> [interval-seconds]` runs the same check as a
daemon (default cadence 60 s, clamp 10..86400). The top bar
chip `watch N` confirms how many are running. `:watch-ref-stop
<ref>` cancels one; `:watch-ref-stop` with no arg cancels all.
The S12 Watchlist screen shows results as they arrive.

### Can I use bee-tui from CI / cron without the TUI?

`bee-tui --once <verb> [args] [--json]` runs a single verb,
prints one line (or JSON), and exits with `0` (ok), `1`
(unhealthy / failed), or `2` (usage error). 24 verbs available
— `readiness`, `inspect`, `durability-check`, `plan-batch`,
`buy-preview`, etc. See [`--once`](../commands/once.md). The
exit codes + JSON shape are part of the semver-stable surface,
so CI gates that depend on them won't break across minor
upgrades.

### Where does the diagnostic bundle go?

`$TMPDIR/bee-tui-diagnostic-<timestamp>.txt`. The status
line prints the full path. Bearer tokens are NEVER captured
— safe to share. See [`:diagnose`](../commands/diagnose.md).

### Can I run `:pins-check` while the cockpit is busy?

Yes. It runs in the background and streams to a file. You
can keep navigating screens. The cockpit won't slow down.
See [`:pins-check`](../commands/pins-check.md).

### My terminal won't render Unicode

`bee-tui --ascii` or set `[ui].ascii_fallback = true` in
config. See [Theme & accessibility](./theme.md).

### How do I see what HTTP calls the cockpit is making?

The bottom log pane underneath every screen — a live tail of
every request, with method / path / status / elapsed. See
[the log-pane page](../screens/s10-log.md) (file kept at its
old `s10-log.md` path for stable links).

### The log pane is too small. Can I make it bigger?

Three options:

- **`+` / `-`** grows / shrinks the bottom pane by one line.
  Range 4–24 lines; the choice persists across launches via
  `~/.local/share/bee-tui`.
- **`Shift+L`** (v1.13+) toggles fullscreen log mode: the
  active screen collapses to zero lines and the log pane
  fills the middle of the cockpit. Press again to return to
  the split layout. Same tabs, same scroll position, same
  filter — just bigger.
- For long-form digging, `:diagnose` and `:loggers` write
  full snapshots to `$TMPDIR/bee-tui-*` files that you can
  open in your editor of choice.

### How do I grep / search inside the log pane?

`/` (v1.13+) opens an inline filter prompt at the top of the
pane. Type a case-insensitive substring; the pane hides every
line that doesn't match. `Enter` commits, `Esc` cancels.
Pressing `Esc` with an active filter and no prompt open
clears the filter. The title strip carries a `/<query> · N
matches` chip so you can confirm at a glance. The filter
follows you across tabs (`[` / `]`), so the same needle
applies whether you're looking at Errors, Bee HTTP, or the
cockpit-own log.

### How do notifications work?

Three sinks (v1.14+), all driven by the same alert diff pipeline
that powers `[alerts]` / `[fleet]` webhooks:

- **In-cockpit toasts** — top-right, max 3 visible, auto-dismiss
  after `[notifications].toast_seconds` (default 8 s). Severity
  colour follows the target gate state (`Fail` red, `Warn` yellow,
  recovery green, `Info` blue). Disable with `toast_enabled = false`.
- **History overlay** — `Ctrl+Alt+N` (or `:notifications`) opens
  a centred ring buffer of the last 200 transitions, newest first.
  Always on, regardless of the other sinks.
- **Desktop notifications** — opt in with `desktop = true`. Uses
  `notify-rust` with its pure-Rust zbus backend, so there's no
  `libdbus` dependency at build or run time. Linux/D-Bus only;
  silently no-ops elsewhere and never panics.
- **Terminal bell** — `bell = "warn"` beeps on Warn/Fail
  transitions; `bell = "fail"` only on Fail. Recoveries never beep.

All four are independent of `[alerts].webhook_url` — webhooks can
stay on for the team channel while toasts + desktop bell handle the
single operator's local console. No sink is required; the history
overlay still records every transition.

## Things the cockpit deliberately won't do

### "Why can't I cash out cheques from the cockpit?"

Cashing is on-chain and costs gas. The cockpit is read-mostly
— mutating endpoints that move money are intentionally off
the keymap so you don't fat-finger them. Use curl + the
`/chequebook/cashout/<peer>` endpoint when you mean it.

### "Why can't I buy stamps from the cockpit?"

Same. Stamp purchase is on-chain, costs gas, has knobs
(depth, amount) you should think about. The cockpit shows
existing stamps' state; the buy itself is `bee postage buy`
or curl.

### "Why can't I configure logging persistently?"

`:set-logger` is runtime-only. Bee's persistent logging
config is in its `config.yaml`; the cockpit doesn't write
to it. By design — you might want push-sync at debug for
30 min, not forever.

### "Why no in-cockpit `connect <peer>`?"

Bee's kademlia handles peer dialing automatically.
Manual `connect` is a debugging escape hatch, not normal
operator behaviour. If you need it, use curl.

## Where to ask the question this FAQ doesn't answer

- GitHub issues for bee-tui: cockpit bugs, doc errors,
  feature requests
- Swarm Discord `#node-operators`: Bee questions that
  aren't cockpit-specific
- bee-rs / bee-py / bee-go for client questions
