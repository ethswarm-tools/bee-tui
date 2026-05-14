# Configuration

bee-tui's configuration is a single TOML file. With no config
at all, the cockpit talks to `http://localhost:1633` against
a node with no auth token — the most common dev setup. As
soon as you have a real Bee node with a Bearer token, or
multiple nodes you want to switch between, you'll want a
config.

## Where the config lives

The fastest way to load a specific file is the `--config` flag
(v1.15.2+) — it points **straight at the file**, no directory
search, no fixed file name:

```sh
bee-tui --config ~/work/bee-nodes.toml
bee-tui --config ./staging.yaml --once readiness   # also works in --once mode
```

The file must exist and end in a recognised extension (`.toml`,
`.json5`, `.json`, `.yaml` / `.yml`, `.ini`); bee-tui errors out
cleanly if it doesn't, rather than silently falling back.

Without `--config`, bee-tui searches an ordered list of
**directories** for a `config.toml` (or `config.json5` / `.json`
/ `.yaml` / `.ini`), taking the first directory that has one:

1. `$BEE_TUI_CONFIG`, if set — **a directory**, not a file path;
   bee-tui looks for `config.toml` *inside* it.
2. `~/.config/bee-tui/` — **on every platform**, including macOS
   and Windows.
3. The platform-native config directory:
   - **Linux**: `$XDG_CONFIG_HOME/bee-tui/` (i.e. `~/.config/bee-tui/`)
   - **macOS**: `~/Library/Application Support/com.ethswarm-tools.bee-tui/`
   - **Windows**: `%APPDATA%\com.ethswarm-tools\bee-tui\`
4. (built-in default — single `local` node, no token)

> **macOS / Windows.** Entry 2 means you can keep your config at
> `~/.config/bee-tui/config.toml` like a Linux user — you do not
> need to touch the platform-native path. (Before v1.15.1 only
> the platform-native directory was searched.)

`bee-tui --version` prints the directory a config was actually
loaded from — or, if none was found, every directory it searched.
That's the fastest way to confirm where to put the file.

The directory does **not** need to exist before launch;
bee-tui only reads, never writes. Create it yourself the
first time — this works on every platform:

```sh
mkdir -p ~/.config/bee-tui
$EDITOR ~/.config/bee-tui/config.toml
```

## Minimal example

```toml
[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"
default = true
```

That's the whole file: one node, named `prod-1`, with its
auth token resolved from `$BEE_TOKEN_PROD1` at startup.

## Schema reference

### `[[nodes]]` — the node array

You can declare any number of `[[nodes]]` entries. Exactly
one should have `default = true`; that's the profile bee-tui
loads on launch. The others are reachable via `:context <name>`.

| Field | Type | Required | Description |
|---|---|---|---|
| `name` | string | yes | Identifier shown in the top bar and used by `:context <name>`. Keep short — `prod-1`, `lab`, `staging`. |
| `url` | string | yes | Base URL of the Bee node, e.g. `http://localhost:1633` or `https://bee.example.com:1633`. Trailing slash optional. |
| `token` | string | no | Bearer token. May be the literal token string, or `@env:VAR_NAME` to resolve from an environment variable at startup. Empty / missing = no auth header sent. |
| `log_file` | path | no | Path to this node's log **file** (v1.15+). When set **and** bee-tui is *not* spawning Bee itself, the cockpit tails this file to populate the bottom log pane's Bee-side tabs (Errors / Warn / Info / Debug / Bee HTTP). Tailing starts at end-of-file — pre-existing history is not replayed. |
| `log_command` | string | no | Shell **command** whose stdout streams this node's log (v1.15+) — `journalctl -u bee -f`, `docker logs -f bee 2>&1`, `ssh host 'tail -f /var/log/bee.log'`. For a Bee whose log file the cockpit can't read directly: remote host, container, restricted permissions. Run via `sh -c`. Takes **precedence** over `log_file` when both are set. |
| `default` | bool | no | If true, this profile is loaded on launch. Exactly one entry should have it. |

**You usually don't need either field for a local node.** If
you set *neither* `log_file` nor `log_command`, bee-tui tries
**auto-discovery** (v1.15+): for a local node it finds the Bee
process listening on the API port and inspects where its
stdout goes — a log file gets tailed directly, a systemd unit
becomes `journalctl -u …`, a docker container becomes
`docker logs -f …`. If it finds a local Bee whose log it
genuinely can't reach (Bee logging to a bare terminal, or to
`/dev/null`), the Bee-side log tabs show a precise explanation
and fix instead of staying silently empty. Auto-discovery is
Linux-only and only works for local nodes — set `log_file` /
`log_command` explicitly for remote nodes, or to override what
discovery picked.

`log_file` and `log_command` are ignored when bee-tui owns the
supervisor (`[bee]` / `--bee-bin`) — the supervised child's
captured log is tailed instead. `:context`-switching follows
the new node's source: the old node's tailer is cancelled and
a fresh one resolved (explicit config, then auto-discovery)
for the new node.

```toml
[[nodes]]
name    = "local"
url     = "http://localhost:1633"          # logs auto-discovered — no log_* needed
default = true

[[nodes]]
name        = "prod-eu"
url         = "https://bee-eu.example.com:1633"
token       = "@env:BEE_TOKEN_EU"
log_command = "ssh bee-eu 'tail -f /var/log/bee/bee.log'"   # remote: must be explicit
```

The `--bee-log <PATH>` and `--bee-log-cmd <CMD>` CLI flags
override `log_file` / `log_command` on the **active** node at
startup — handy for a one-off without editing config.
`--bee-log-cmd` takes precedence over `--bee-log`. They do not
affect other `[[nodes]]` entries; subsequent `:context`
switches use each node's own config. Both are ignored when
`--bee-bin` is set (supervisor mode wins).

> **stderr.** The command tailer follows **stdout** only. Log
> sources that write to stderr (notably `docker logs`) need a
> `2>&1` redirect in the command string — `sh -c` handles it.

### `[ui]` — UI preferences

```toml
[ui]
theme           = "default"
ascii_fallback  = false
```

| Field | Type | Default | Description |
|---|---|---|---|
| `theme` | `"default"` \| `"mono"` | `"default"` | Slot-based palette. `default` is vibrant green/yellow/red. `mono` is greyscale only — useful on terminals where colour is muted or distracting, or when piping to a recording tool that doesn't preserve colour. |
| `ascii_fallback` | bool | `false` | If true, every component renders ASCII glyphs (`OK / X / ! / > / # / .`) instead of Unicode (`✓ ⚠ ✗ ▶ ▇ ░`). Equivalent to passing `--ascii` on the command line. Use on Windows Terminal pre-Win11, screen readers, or SSH chains that mangle Unicode. |
| `refresh` | `"live"` \| `"default"` \| `"slow"` | `"default"` | Polling cadence preset. `live` matches the original 2 s health / 5 s topology+tags rates (chatty; use when actively diagnosing). `default` doubles the fast-tier intervals (4 s / 10 s) — about half the request volume, no perceptible loss for monitoring. `slow` is minimal (8 s / 20 s / 60 s / 120 s) for leave-it-open-all-day operators. |

Unknown values for `theme` fall back to `"default"` with a
single tracing warning so a typo doesn't break startup.

### `[bee]` — spawn Bee from bee-tui (optional)

When set, bee-tui launches Bee itself before opening the
cockpit, captures its stdout + stderr to `$TMPDIR/bee-tui-spawned-<ts>.log`,
waits for `/health` to respond, then enters the TUI. Quit
sends SIGTERM to Bee's process group; a 5-second grace window
is followed by SIGKILL if needed.

```toml
[bee]
bin    = "/home/operator/bee/dist/bee"
config = "/home/operator/bee/testnet.yaml"
```

| Field | Type | Required | Description |
|---|---|---|---|
| `bin` | path | yes | Path to a `bee` binary. Bee is invoked as `<bin> start --config <config>`. Relative paths resolve against the working directory. |
| `config` | path | yes | Path to the Bee YAML config the binary should be started with. |

If `[bee]` is **omitted**, bee-tui falls back to its legacy
mode: connect to whatever's already running on the URL of the
default `[[nodes]]` entry. Use this when Bee runs under
systemd / docker / k8s — bee-tui shouldn't spawn it then.

If Bee crashes mid-session, a red `bee exited (code N)` chip
appears in the top bar. Auto-restart is opt-in via the
`[bee.supervisor]` subsection below — without it, the operator
decides whether to investigate (the captured log is the place
to start) or quit and relaunch.

CLI flags `--bee-bin` and `--bee-config` override the
`[bee]` block. Both must be set together; setting only one
errors at startup.

### `[bee.supervisor]` — auto-restart watchdog (v1.12+, optional)

When `auto_restart = true`, bee-tui relaunches the supervised
Bee process after it exits (any reason — clean exit, signal,
OOM). Exponential backoff between attempts protects against
fast crashloops; a sliding one-hour budget caps the noise if
something fundamentally broken keeps the binary from staying
up.

```toml
[bee.supervisor]
auto_restart           = true   # default: false (no restart)
max_restarts_per_hour  = 6      # sliding window budget
backoff_initial_secs   = 1      # doubles on each attempt
backoff_max_secs       = 30     # cap on backoff
```

| Field | Type | Default | Description |
|---|---|---|---|
| `auto_restart` | bool | `false` | When true, bee-tui re-spawns Bee after the child exits. |
| `max_restarts_per_hour` | u32 | `6` | Maximum restart attempts within a rolling one-hour window. Once hit, the watchdog stops respawning until the window slides forward. |
| `backoff_initial_secs` | u64 | `1` | Wait before the first restart attempt; doubles after each. |
| `backoff_max_secs` | u64 | `30` | Hard cap on the exponential backoff. |

With the watchdog active the top-bar chip changes from the
old "show only on crash" behaviour to an always-visible
`bee running 4d3h (2 restarts)` style label, so operators see
both uptime and historical restart count at a glance. When
the budget is exhausted the chip reads `bee: max restarts
(6/6) hit` — an operator-actionable signal that "this isn't
recovering on its own."

### `[metrics]` — Prometheus scrape endpoint (optional)

```toml
[metrics]
enabled = true
addr    = "127.0.0.1:9101"   # default; only opt into 0.0.0.0 if you mean it
```

Off by default. When enabled, bee-tui serves Prometheus
exposition-format gauges on the configured address — the unique
synthesised metrics (worst-bucket per batch, depth-vs-radius gap,
predicted TTL, pending-tx age, bee-tui's own request percentiles)
that Bee's own `/metrics` doesn't expose. See the [Prometheus
metrics reference](./reference/metrics.md) for the full list.

### `[economics]` — cost-context oracles (optional)

```toml
[economics]
gnosis_rpc_url      = "https://rpc.gnosischain.com"   # required by :basefee + Market tile gas line
enable_market_tile  = true                            # default false; turns on the S3 SWAP Market tile
```

Two facets:

- **Verbs** (`:price`, `:basefee`) work without the section being
  present — `:price` always hits the public Swarm token service;
  `:basefee` errors with a clear "configure
  `[economics].gnosis_rpc_url`" hint when unset.
- **Market tile** on S3 SWAP is opt-in via `enable_market_tile = true`.
  When on, bee-tui polls `tokenservice.ethswarm.org` (and, if
  `gnosis_rpc_url` is set, the Gnosis RPC) every 60 s and renders a
  one-line tile showing `BZZ ≈ $X.XXXX` and `gas: B base + T tip = N
  gwei`. Off by default — fresh installs make no outbound traffic.

### `[durability]` — chunk-graph walker tuning (optional)

```toml
[durability]
swarmscan_check  = true                                          # default false
swarmscan_url    = "https://api.swarmscan.io/v1/chunks/{ref}"   # default
```

Off by default — fresh installs make no outbound traffic to a
third-party indexer. When `swarmscan_check = true`, every
completed `:durability-check` (single-shot or via the
`:watch-ref` daemon) probes `swarmscan_url` for an independent
"does the network see this ref?" answer. The literal `{ref}`
substring in the URL template is replaced with the hex-encoded
reference at request time; the probe times out after 5 s.

The result lands in `DurabilityResult.swarmscan_seen` and shows
up in:

- The verb's summary line: `swarmscan: seen` /
  `swarmscan: NOT seen` (or omitted when the probe was skipped or
  errored).
- The S12 Watchlist row detail: ` · scan: seen` /
  ` · scan: NOT seen`.
- `--once durability-check`'s JSON: `swarmscan_seen` field
  (`true` / `false` / `null`).

A `NOT seen` answer doesn't flip the `is_healthy()` flag — it's
an independent signal, useful for catching cases where the local
node returns a chunk from cache that no peer in the network
actually still has. Pair with `[alerts].webhook_url` to ping on
gate transitions and use `swarmscan_seen` as a manual sanity
check.

### `[pubsub]` — pubsub history file + rotation (optional)

```toml
[pubsub]
history_file   = "/var/lib/bee-tui/pubsub.jsonl"  # off by default
rotate_size_mb = 64    # active file rolls over at this size; 0 disables (default 64)
keep_files     = 5     # retain .1 .. .5; older rotations unlinked (default 5)
```

Off by default — fresh installs don't write any pubsub messages
to disk. When `history_file` is set, every PSS / GSOC frame
delivered to S15 is also appended to the JSONL file (one
JSON-encoded message per line) so overnight subscriptions can be
analysed offline. The file is created with mode `0600`
(owner-only) since payloads can be sensitive on multi-user hosts.

Rotation keeps disk usage bounded. When the active file crosses
`rotate_size_mb` MiB, bee-tui renames it to `<path>.1` (older
rotations shift to `.2`, `.3`, …, `.keep_files`; oldest beyond
`keep_files` is unlinked) and re-opens `<path>` empty.
Concurrent watchers serialise through the same mutex that orders
appends, so no rename races. Set `rotate_size_mb = 0` to disable
rotation (file grows unbounded).

Pair with [`:pubsub-replay <path>`](./commands/bar.md) to load a
prior session's JSONL back into S15 for visual analysis without
restarting any subscription.

### `[alerts]` — webhook ping when a health gate flips (optional)

```toml
[alerts]
webhook_url    = "https://hooks.slack.com/services/T000/B000/XXX"
debounce_secs  = 300   # default; per-gate cool-down so a flapping gate doesn't pin Slack
```

Off by default — without `webhook_url`, no outbound traffic. When
set, every health-gate transition (e.g. `Reachability: Pass → Fail`,
`StorageRadius: Warn → Pass`, `Stamp TTL: Pass → Warn` when a batch
crosses the 7-day topup-planning threshold) becomes one POST with a
Slack/Discord-compatible `{"text": "..."}` body. Transitions to or
from `Unknown` (data-not-loaded-yet) are suppressed so cockpit
startup never spams the channel. After firing for gate X, no further
alert for X until `debounce_secs` elapses, regardless of how many
times that gate flapped in between.

### `[fleet]` — fleet-aggregate webhook (v1.12+, optional)

Consolidates per-node alerts across the S15 Fleet into a
single rolled-up POST, so operators running 5+ nodes don't
get five individual Slack pings during a network blip.
Sits **on top of** per-node `[alerts].webhook_url` — both
keep working independently when both are set.

```toml
[fleet]
aggregate_webhook_url   = "https://hooks.slack.com/services/T000/B000/YYY"
aggregate_window_secs   = 60        # batch alerts within this window
```

| Field | Type | Default | Description |
|---|---|---|---|
| `aggregate_webhook_url` | string \| absent | absent | Slack / Discord-compatible incoming-webhook URL. When unset, no fleet-aggregate alerts; per-node `[alerts]` is unaffected. |
| `aggregate_window_secs` | u64 | `60` | Coalesce window. Status transitions across all nodes are batched within this window into one message. |

On every fleet-poll tick (every 10 seconds) bee-tui ingests
the new snapshot, notes any nodes that **transitioned** to a
worse status (`Pass → Warn / Fail`, `Warn → Fail`) or
**recovered** (`Fail → Pass / Warn`), and buffers them. Once
the coalesce window elapses, the buffer is drained and one
POST goes out with a body like:

```
Fleet alert: 2 fail · 1 warn
• prod-eu: Pass → Fail (unreachable)
• staging: Pass → Fail (0 peers — isolated)
• prod-us: Pass → Warn (stamp TTL under 7d — plan a topup)
```

Steady-state failures don't re-alert — once a node is
recorded as `Fail`, you only get another message when it
moves (recovers, or steps up to a different state). Cold
start (`Unknown → anything`) is suppressed for the same
reason `[alerts]` suppresses it: the cockpit just learning a
real value isn't an alertable event.

### `[notifications]` — in-cockpit notification center (v1.14+, optional)

Surface every alert transition the cockpit observes — the same
ones [`[alerts]`](#alerts--webhook-ping-when-a-health-gate-flips-optional)
and [`[fleet]`](#fleet--fleet-aggregate-webhook-v112-optional) gate
behind webhooks — without leaving the terminal. Three sinks, each
opt-in:

```toml
[notifications]
toast_enabled  = true   # default; top-right ephemeral toast on every transition
toast_seconds  = 8      # default; how long each toast lingers
desktop        = false  # libnotify / D-Bus desktop notifications (Linux)
bell           = "off"  # "off" | "warn" | "fail" — terminal bell threshold
```

| Field | Type | Default | Description |
|---|---|---|---|
| `toast_enabled` | bool | `true` | Show top-right toasts (max 3 visible). Disable to keep just the history overlay + optional desktop / bell. |
| `toast_seconds` | u64 | `8` | Toast lifetime before auto-dismiss. |
| `desktop` | bool | `false` | Forward `Fail` / `Warn` transitions to the OS notification center (`notify-rust` with the pure-Rust zbus backend — no `libdbus` dependency). Silently no-ops if no D-Bus session is reachable; never panics. |
| `bell` | string | `"off"` | Terminal-bell threshold. `"off"` never beeps. `"warn"` beeps on `Warn`+`Fail`. `"fail"` beeps on `Fail` only. Recovery transitions never beep. |

Regardless of these settings, the **notification history** overlay
(`Ctrl+Alt+N` or `:notifications`) always shows the last 200
transitions newest-first — that ring buffer is unconditional, so
you can disable every sink above and still have an in-cockpit audit
log of every gate flip the session has seen.

The history overlay and toasts share their source with `[alerts]` and
`[fleet]`: same diff pipeline, just a different sink. You can run all
three in parallel — webhook + toast + desktop — or any subset.

### CLI overrides

Three command-line flags override the config file:

```sh
bee-tui --ascii        # forces ascii_fallback = true
bee-tui --no-color     # forces theme = "mono"
NO_COLOR=1 bee-tui     # same as --no-color, per <https://no-color.org>
```

Resolution order (highest priority first):

1. `--ascii` flag → ascii glyphs (regardless of config)
2. `--no-color` flag OR `NO_COLOR` env (any non-empty value) → mono palette
3. `[ui].ascii_fallback` from config → ascii glyphs
4. `[ui].theme` from config → palette

## The `@env:VAR` token form

Every Bee API endpoint that's not explicitly public requires
a Bearer token. Hard-coding the token in `config.toml` is
fine for a lab node, but for production it's the wrong shape —
the file lands in dotfiles backups, screenshots, support
threads, etc. The `@env:VAR` form keeps the token out of
the file:

```toml
token = "@env:BEE_TOKEN_PROD1"
```

bee-tui reads `$BEE_TOKEN_PROD1` **once at startup** and uses
the resolved value for every request. The literal string
`@env:BEE_TOKEN_PROD1` is never logged, never captured in
`:diagnose` bundles, never sent to Bee. If the variable is
unset, bee-tui logs a tracing warning and proceeds without
an auth header (the request will then 401).

You can mix forms across nodes — one `@env:` and one literal
in the same config is fine.

## Multi-node setups

```toml
[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"
default = true

[[nodes]]
name  = "prod-2"
url   = "http://10.0.1.6:1633"
token = "@env:BEE_TOKEN_PROD2"

[[nodes]]
name = "lab"
url  = "http://localhost:1633"

[ui]
theme = "default"
```

Launch picks `prod-1` (the default). At runtime, switch with:

- `:context prod-2` — swap to the second prod node
- `:context lab` — swap to the local lab node
- `:context` — list every configured profile name

The switch is fast (no restart) but **not stateful across
launches** — every run starts on the `default = true`
profile.

See [`:context`](./commands/context.md) for the deep dive
on what's preserved vs reset on switch.

## Validating your config

If bee-tui fails to start with a config error, the message is
the first thing on stderr — common ones:

| Error | Fix |
|---|---|
| `no Bee node configured (config.nodes is empty)` | Add at least one `[[nodes]]` entry. |
| `no default node selected` | Mark exactly one `[[nodes]]` with `default = true`. |
| `invalid url: …` | Quote URLs that contain ports: `url = "http://10.0.1.5:1633"` (TOML accepts unquoted in a TOML number-shape unrelated to URLs, leading to confusing parses). |
| `unknown theme name "X" — falling back to default` | Just a warning; not fatal. Set `theme` to `"default"` or `"mono"`. |

To dump the resolved config for debugging:

```sh
:diagnose
```

The bundle in `$TMPDIR/bee-tui-diagnostic-<ts>.txt` includes
the active profile name and endpoint URL. Tokens are never
captured — they live in HTTP headers, not URLs.
