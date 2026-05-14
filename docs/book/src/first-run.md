# First run

This page walks through what an operator sees the first time
they launch bee-tui against a Bee node — the loading shapes,
the warmup behaviour, and the few key bindings worth
internalising before reading the per-screen pages.

## Launch

With no config file, bee-tui talks to `http://localhost:1633`
out of the box:

```sh
bee-tui
```

To point at a different node — or several at once, or a
specific config file — see [Launching bee-tui](./launching.md)
for every invocation mode. The rest of this page assumes the
plain `bee-tui` launch above.

The cockpit takes over the terminal in *alt-screen* mode —
your shell prompt is preserved underneath and restored on
quit. If alt-screen doesn't work (e.g., piped output, no
TTY), the binary errors out cleanly with a one-line message
rather than scribbling escape sequences into your scrollback.

## What you see in the first second

```
 bee-tui   local @ http://localhost:1633   ping —   UTC HH:MM:SS
 [Health]  Stamps  Swap  Lottery  Peers  Network  Warmup  API  Tags  Pins  Manifest  Watchlist  FeedTimeline  Pubsub    :cmd · Tab · ? help
─────────────────────────────────────────────────────────────────────────────────────
HEALTH   local · http://localhost:1633     ping: —ms
 ⠋ loading…

 ·  API reachable                loading…
 ·  Chain RPC                    loading…
 ·  Wallet funded                loading…
 …
─────────────────────────────────────────────────────────────────────────────────────

┌ bee::http ──────────────────────────────────────────────────────────────────────────┐
│                                                                                     │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

A clean first launch shows only the four "always-on" header
fields. As soon as something is running in the background,
v1.10+ appends awareness chips after the `ping` block:

```
 bee-tui   local @ http://localhost:1633   ping 4ms   UTC HH:MM:SS   subs 2   watch 1   alerts ●   notif 3
```

- `subs N` — active PSS / GSOC subscriptions (see S15 and the
  `:pubsub-pss` / `:pubsub-gsoc` verbs).
- `watch N` — active `:watch-ref` daemons (see S13).
- `alerts ●` — present whenever `[alerts].webhook_url` is set
  in `config.toml`; the green dot confirms outbound pinging is
  configured even when no alerts are firing.
- `notif N` (v1.15+) — notifications that have arrived since you
  last opened the `Ctrl+Alt+N` history overlay. Warn-coloured so
  it's noticeable; cleared to zero when you open the overlay.
  Toasts auto-dismiss, so this is the persistent "have I seen
  everything?" cue.

Each chip is hidden when its count is zero (or `alerts` isn't
configured), so the header stays calm on a fresh session and
visibly busy when daemons are running.

Three things are happening in parallel:

1. **The watch hub is firing first requests.** Each screen has
   one or more endpoint pollers; the cadence is per-resource
   (2 s for health, 5 s for topology, 30 s for swap, etc.).
2. **The spinner glyph in the header is rotating** (`⠋ ⠙ ⠹ ⠸ ⠼ ⠴ ⠦ ⠧ ⠇ ⠏`) once per tick. If you see the
   spinner moving, the redraw loop is alive even if Bee
   hasn't responded yet.
3. **The bottom bee::http strip is empty** until the first
   request lands. The first tick that sees an HTTP response
   appends a line.

## What "503 syncing" looks like

If you launch bee-tui against a Bee node that's still in its
own warmup, most endpoints return:

```
HTTP 503: Node is syncing. This endpoint is unavailable. Try again later.
```

bee-tui detects this case specifically and renders it on the
header line in **yellow (warn)** rather than red:

```
syncing — Bee is still bootstrapping; this view will populate once it catches up
```

That tooltip is in `theme::classify_header_error`. You don't
need to do anything; the screen will populate as soon as Bee
finishes its bootstrap.

## What "Bee is unreachable" looks like

If Bee isn't running, or `localhost:1633` is wrong, you'll see:

```
error: TCP connect failed: Connection refused
```

This **is** rendered red — it's a real problem, not transient.
Common fixes:

- Bee isn't running: start it (`./bee start --config <path>`)
- Bee is on a different host: edit `~/.config/bee-tui/config.toml`
  to point at the right URL
- Auth token wrong / missing: see [Configuration](./config.md)
  for the `@env:VAR` token form

## What you should do during warmup (S5)

If you launched bee-tui *while* your Bee node was still in
its 25–60 minute cold start, the most useful screen is **S5
Warmup**. Tab to it (or type `:warmup`). It's a five-step
checklist:

1. Postage snapshot loaded
2. Peer bootstrap (against ~50 peers)
3. Kademlia depth stable (5-tick window)
4. Reserve fill (`reserve_size_within_radius / 65,536`)
5. Stabilization (terminal step keyed on `is_warming_up=false`)

The screen freezes the elapsed counter the moment Bee flips
`is_warming_up` to false, so you can come back later and see
how long the warmup actually took.

## Keyboard basics

Internalise these five keys:

| Key | Effect |
|-----|--------|
| `Tab` | Cycle to the next screen |
| `:` | Open the command bar |
| `?` | Toggle the per-screen help overlay |
| `↵` | Drill (S2 batches, S6 peers — when a row is selected) |
| `q` / `Ctrl+C` | Quit |

Everything else is per-screen and lives in the `?` overlay.

## What's typical, what's not

After ~30 seconds against a healthy mainnet node:

- **S1 Health**: ten gates, mostly green ✓ checkmarks. One or
  two warns (`⚠`) is normal — bin saturation flickers, chain
  RPC may show `Δ +1` block lag.
- **S6 Peers**: 80–150 connected peers across a dozen-ish
  bins. The first two or three bins should be Healthy. Far
  bins (depth+5 onward) being Empty is expected.
- **S2 Stamps**: usually 1–3 batches. Worst-bucket fill
  percentage is the headline number to watch — anything
  above 80 % is Skewed (yellow), above 95 % is Critical (red).
- **Bottom log pane** (always visible): a constant stream of `GET /status` /
  `GET /chainstate` / etc. every 1–2 seconds. If this strip
  goes silent for >5 seconds, something is wrong with the
  cockpit (or your network), not with Bee.

If S1's "Bin saturation" gate is `STARVING`, that's the most
common operator pain point — see the [S6 Peers](./screens/s6-peers.md)
page for what to do about it.

## Quitting

`q` quits cleanly. So does `Ctrl+C`. Both:

- Cancel every in-flight HTTP request (the hierarchical
  `CancellationToken` propagates from the root)
- Restore your terminal from alt-screen
- Persist nothing implicitly — if you ran `:context` to
  switch profiles, that switch isn't sticky across launches;
  the `default = true` profile in `config.toml` always
  wins on next launch
