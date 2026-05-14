# bee-tui

[![Crates.io](https://img.shields.io/crates/v/bee-tui.svg)](https://crates.io/crates/bee-tui)
[![CI](https://github.com/ethswarm-tools/bee-tui/workflows/CI/badge.svg)](https://github.com/ethswarm-tools/bee-tui/actions)
[![Docs](https://img.shields.io/badge/docs-mdBook-blue)](https://ethswarm-tools.github.io/bee-tui/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

> **Operator handbook**: <https://ethswarm-tools.github.io/bee-tui/> — full
> per-screen reference, command bar, keymap, FAQ.

A k9s-style terminal cockpit for [Ethereum Swarm](https://www.ethswarm.org/) Bee
node operators — fifteen live screens that surface the state Bee's API hides:
bucket collisions, redistribution skip reasons, bin starvation, NAT reality,
manifest contents, durability checks, feed timelines, pubsub watches, and a
multi-node fleet roll-up — plus a live HTTP tail so operators trust what they see.

![bee-tui cold-start tour](docs/tapes/cold-start.gif)

```text
 bee-tui   prod-1 @ http://10.0.1.5:1633   ping 12ms   UTC 14:32:18
 [Health]  Stamps  Swap  Lottery  Peers  Network  Warmup  API  Tags  Pins  Manifest  Watchlist  FeedTimeline  Pubsub    :cmd · Tab · ? help
─────────────────────────────────────────────────────────────────────────────────────
HEALTH   prod-1 · http://10.0.1.5:1633     ping: 8ms

 ✓  API reachable                /health 200 in 3ms
 ⚠  Chain RPC                    block 8412930 · Δ +1
 ✓  Wallet funded                BZZ 27.97 · native 5.02
 ✓  Warmup complete              ready
 ✓  Peers                        87 connected
 ✓  Reserve                      65,536 chunks (in-radius: 65,536) · radius 8
 ⚠  Bin saturation               2 starving: bin 4, bin 5
        └─ manually `connect` more peers or wait — kademlia fills bins…
 ✓  Healthy for redistribution   yes
 ✓  Not frozen                   yes
 ✓  Sufficient funds to play     yes
 ✓  Stamp TTL                    worst-batch a1b2c3d4 · TTL 14d 3h
─────────────────────────────────────────────────────────────────────────────────────
:cmd
┌ bee::http ──────────────────────────────────────────────────────────────────────────┐
│ 14:32:18  GET   /status                  200    3ms                                 │
│ 14:32:18  GET   /redistributionstate     200  104ms                                 │
│ 14:32:18  GET   /chainstate              200    0ms                                 │
└─────────────────────────────────────────────────────────────────────────────────────┘
```

## Install

**Prebuilt installer** (Linux / macOS / Windows — no Rust toolchain required):

```sh
# Linux / macOS — picks the right tarball for your arch
curl --proto '=https' --tlsv1.2 -LsSf \
  https://github.com/ethswarm-tools/bee-tui/releases/latest/download/bee-tui-installer.sh \
  | sh

# Windows (PowerShell)
powershell -c "irm https://github.com/ethswarm-tools/bee-tui/releases/latest/download/bee-tui-installer.ps1 | iex"
```

Or grab a tarball directly from the [releases page](https://github.com/ethswarm-tools/bee-tui/releases).
Builds are produced for `aarch64-apple-darwin`, `x86_64-apple-darwin`,
`aarch64-unknown-linux-gnu`, `x86_64-unknown-linux-gnu`, and
`x86_64-pc-windows-msvc`.

**From source** (needs Rust ≥ 1.85):

```sh
cargo install bee-tui
```

## Quickstart

Point bee-tui at a running Bee node:

```sh
# Default config talks to http://localhost:1633
bee-tui

# Point at one or more nodes by URL — no config file needed (v1.16+).
# First URL is the active node; all of them appear in Ctrl+N + S16 Fleet.
bee-tui http://localhost:1633
bee-tui http://localhost:1633 https://bee-eu.example.com:1633 https://bee-us.example.com:1633

# Point at a specific config file (v1.15.2+) — straight at the file
bee-tui --config ~/work/bee-nodes.toml

# Or set the config *directory* via env (BEE_TUI_CONFIG is a dir, not a file)
BEE_TUI_CONFIG=~/.config/bee-tui bee-tui

# Force ASCII glyphs (Windows Terminal pre-Win11, screen readers, broken SSH chains)
bee-tui --ascii

# Suppress colour (also honours NO_COLOR env per <https://no-color.org>)
bee-tui --no-color

# Bee logs in the bottom pane (v1.15+): for a LOCAL node, just run `bee-tui` —
# it auto-discovers where Bee's log goes (file / systemd journal / docker).
# For a remote node, or to be explicit, point it at a file or a command:
bee-tui --bee-log /var/log/bee/bee.log              # a log file
bee-tui --bee-log-cmd "journalctl -u bee -f"        # ...or a command's stdout
bee-tui --bee-log-cmd "ssh bee-host 'tail -f /var/log/bee.log'"   # ...remote
```

A minimal `~/.config/bee-tui/config.toml`:

```toml
[[nodes]]
name    = "prod-1"
url     = "http://10.0.1.5:1633"
token   = "@env:BEE_TOKEN_PROD1"   # resolves at startup; never logged
default = true

[[nodes]]
name = "lab"
url  = "http://localhost:1633"          # local node — Bee logs auto-discovered (v1.15+)

[ui]
theme           = "default"        # "default" | "mono"
ascii_fallback  = false            # true = same as `--ascii`
```

The `@env:VAR` token form keeps Bearer tokens out of the config file. With the
`local` default profile, `bee-tui` works with zero config against a local Bee.

## What you get

Fourteen operator screens plus an always-on command-log pane:

| Screen | What it answers |
|---|---|
| **S1 Health** | "Why is my node unhealthy?" — 11 gates with WHY tooltips encoding tribal knowledge (e.g. "storageRadius decreases ONLY on the 30-min reserve worker tick"). Includes the v1.4 Stamp TTL gate that fires before batches actually expire, and the v1.4 webhook alerter that POSTs per-gate transitions to Slack / Discord. |
| **S2 Stamps** | "Which batch is about to fail uploads?" — worst-bucket fill bar, immutable-vs-mutable rejection semantics (bee#5334), 5-state status ladder. **`↵` drills into the per-bucket fill histogram.** |
| **S3 Swap / cheques** | Chequebook headroom (with on-chain contract address), per-peer net (received − sent) with `\|net\| > 0.5 BZZ` flagging, last-received cheque table. Optional `[economics].enable_market_tile` adds an xBZZ→USD + Gnosis basefee tile. |
| **S4 Lottery** | "Why am I not earning rewards?" — round timeline, anchor summary (last won / played / selected / frozen with `Δ`), stake card with frozen / unhealthy / insufficient-gas reasoning, on-demand `r`-key rchash benchmark. |
| **S5 Warmup** | "What's Bee actually doing during the 25–60-minute cold start?" — five-step checklist with a depth-stability window. |
| **S6 Peers** | Bin saturation strip (Empty / Starving / Healthy / Over) anchored on bee-go's `SaturationPeers=8` and `OverSaturationPeers=18` constants — surfaces the bin-starvation gap no other tool derives. **`↵` drills into per-peer balance / cheques / settlement / ping.** |
| **S7 Network / NAT** | "Why am I unreachable?" — public-vs-private underlay classification, AutoNAT reachability with stability window (flickers under symmetric NAT). |
| **S8 RPC / API health** | Bee API call stats (p50 / p99 latency, error rate over the last 100 calls), pending operator transactions with full tx hash + to-address per row. |
| **S9 Tags / uploads** | "Where is my upload stuck?" — per-tag lifecycle counters (split → sent → synced) and a TagStatus ladder. Long lists scroll with `j k` / `PgUp PgDn` / `Home`. |
| **S10 Pins** | Pinned-reference inspector with per-pin drill (pin detail). Pair with `:pins-check` for a chunk-level integrity walk. |
| **S11 Manifests** | Mantaray tree browser. `:manifest <ref>` or `:inspect <ref>` opens the tree; `↵` lazy-loads forks. The first screen that gives operators X-ray vision into their *data*, not just their node. |
| **S12 Watchlist** | Rolling history of `:durability-check` results — the operator-facing answer to "is my data still alive?" Walks the chunk graph rooted at `<ref>`, BMT-verifies every chunk, optionally cross-checks against swarmscan. |
| **S13 Feed Timeline** | Walks a feed's history (newest first, bounded-parallel) via `:feed-timeline <owner> <topic> [N]`. Default 50 entries, hard cap 1 000. |
| **S14 Pubsub watch** | Live tail of PSS topic + GSOC subscriptions, merged timeline. `:pubsub-pss / :pubsub-gsoc` open subscriptions; `:pubsub-filter` narrows the timeline; `[pubsub].history_file` persists every frame to JSONL with size-based rotation; `:pubsub-replay <path>` loads prior sessions. |
| **S15 Fleet view** | Multi-node health roll-up across every `[[nodes]]` entry — one row per node, polled every 10 s in parallel. Aggregate status + peers + worst stamp TTL + `/health` ping; `Enter` switches context to that node for per-screen detail. |
| **Bottom log pane** | Always-visible `bee::http` request tail (lazygit-style) plus six other tabs (Errors / Warn / Info / Debug / Bee HTTP / Cockpit). The trust anchor — operators learn the API by watching it. |

## Drill panes

Two screens have a per-row drill that fans out to the relevant Bee endpoints
on demand. Both follow the same UX: `↑↓` / `j k` move selection, `↵`
opens the drill, `Esc` closes it.

- **S2 Stamps** drill: fetches `/stamps/{id}/buckets` and renders a
  fill-percentage histogram (six bins from `0%` to `100%`) plus the top
  10 worst buckets. Two batches with the same headline `utilization`
  can fail uploads under wildly different conditions — the drill answers
  *how concentrated* the load is.

  ![S2 stamp drill](docs/tapes/s2-stamp-drill.gif)

- **S6 Peers** drill: parallel fetch of `peer_balance` + `peer_cheques`
  + `peer_settlement` + `ping_peer` for the selected peer. Each field
  fails independently — a 404 on `/chequebook/cheque/{peer}` (peers
  you've never exchanged cheques with) shows `error: 404` for that one
  field instead of blanking the drill.

  ![S6 peer drill](docs/tapes/s6-peer-drill.gif)

## Data screens (v1.2+)

The cockpit grew an "audit" tier across v1.2-v1.9 — screens that
inspect the *data*, the *network's view of the data*, and the
*pubsub messages* flowing through it.

- **S11 Manifests** — `:manifest <ref>` or `:inspect <ref>` opens
  the Mantaray tree; `↵` lazy-loads forks one chunk at a time.

  ![S12 manifest browser](docs/tapes/s12-manifest.gif)

- **S12 Watchlist** — `:durability-check <ref>` walks the chunk
  graph and records the result with BMT verification + optional
  swarmscan cross-check. `:watch-ref <ref> [interval]` runs it as
  a daemon.

  ![S13 durability watchlist](docs/tapes/s13-durability.gif)

- **S13 Feed Timeline** — `:feed-timeline <owner> <topic> [N]`
  walks a feed's update history (newest first, bounded-parallel).

  ![S14 feed timeline](docs/tapes/s14-feed-timeline.gif)

- **S14 Pubsub watch** — `:pubsub-pss` / `:pubsub-gsoc` open
  WebSocket subscriptions; `:pubsub-filter` narrows the merged
  timeline; `:pubsub-replay <path>` reloads a prior session's
  JSONL history file.

  ![S15 pubsub watch](docs/tapes/s15-pubsub.gif)

## CI mode (v1.3+)

`bee-tui --once <verb> [args…] [--json]` runs a single verb without
launching the TUI. 24 verbs across pure-local, Bee-API, and stamp
economics; exit codes `0` (ok) / `1` (unhealthy) / `2` (usage error).
Designed for cron, monitoring, and CI gates.

![--once CI mode](docs/tapes/once-ci.gif)

## Keys

| | |
|---|---|
| `Tab` / `Shift+Tab` | cycle screens forward / backward |
| `1`-`9` | jump to S1 – S9 (Health … Tags) |
| `0` | jump to S10 (Pins) |
| `Alt+1` – `Alt+5` | jump to S11 – S15 (Manifest, Watchlist, FeedTimeline, Pubsub, Fleet) |
| `Ctrl+N` | open the node picker (also `:nodes`) |
| `Ctrl+Alt+N` | open the notification history overlay (v1.14+, also `:notifications`) |
| `Shift+E` | open the batch-economics modal (v1.12+) |
| `Shift+L` | toggle fullscreen log pane (v1.13+) |
| `/` | filter the log pane (v1.13+) — case-insensitive substring |
| `?` | toggle the paged help overlay (Keys ↔ Verbs via `Tab`) |
| `:` | open command bar |
| `qq` | quit (double-tap within ~1.5s; `:q` also works) |
| `Ctrl+C` / `Ctrl+D` | quit immediately (escape hatch) |
| `↑↓` / `j k` | move selection (S2, S6, S11, S12-S15) or scroll (S9) |
| `↵` | drill selected row (S2, S6, S11) / expand fork (S12) |
| `Esc` | close drill / overlay |
| `r` | run rchash benchmark (S4 Lottery) |
| `PgUp` / `PgDn` / `Home` | page through long lists (S9, S13-S15) |

`?` is the source of truth for screen-specific keymaps — every screen advertises
its keys in the overlay. Press `Tab` while help is open to switch to the
**Verbs** page — every `:verb` grouped by category (added v1.10).

**Copying values out of the cockpit:** mouse mode is off by default, so your
terminal's native selection works. Click-drag a peer overlay or batch ID,
then paste it into a block explorer / Discord / etc. — bee-tui doesn't
intercept it.

### Command bar

The cockpit now ships 54 verbs. The headline categories — see the
v1.10 **Verbs** page in `?` for the full catalogue:

| Category | Examples |
|---|---|
| **Navigate** | `:health`, `:stamps`, `:swap`, `:lottery`, `:peers`, `:network`, `:warmup`, `:api`, `:tags`, `:pins`, `:watchlist`, `:manifest <ref>`, `:feed-timeline <owner> <topic>`, `:pubsub` |
| **Inspect** | `:inspect <ref>`, `:hash <path>`, `:cid <ref>`, `:depth-table`, `:feed-probe <owner> <topic>`, `:grantees-list <ref>` |
| **Stamps & economics** | `:topup-preview`, `:dilute-preview`, `:extend-preview`, `:buy-preview`, `:buy-suggest`, `:plan-batch`, `:price`, `:basefee` |
| **Uploads** | `:probe-upload <batch>`, `:upload-file <path> <batch>`, `:upload-collection <dir> <batch>` |
| **Durability** | `:durability-check <ref>`, `:watch-ref <ref> [interval]`, `:watch-ref-stop [ref]`, `:pins-check` |
| **Pubsub** | `:pubsub-pss <topic>`, `:pubsub-gsoc <owner> <id>`, `:pubsub-stop [sub-id]`, `:pubsub-filter <substr>`, `:pubsub-filter-clear`, `:pubsub-replay <path>` |
| **Mining / addresses** | `:gsoc-mine <overlay> <id>`, `:pss-target <overlay>` |
| **Diagnostics & config** | `:check-version`, `:config-doctor`, `:diagnose`, `:loggers`, `:set-logger <expr> <level>` |
| **Cockpit** | `:context <name>` (alias `:ctx`), `:nodes` (or `Ctrl+N`), `:quit` (alias `:q`) |

## Multi-node

Define multiple `[[nodes]]` in `config.toml`. The default profile loads at
startup. Three ways to switch / monitor:

- **S15 Fleet view** (v1.11+) — `Alt+5` opens a simultaneous health roll-up
  across every configured node, polled every 10 s in parallel; `Enter` on a
  row switches context to that node for full per-screen detail.
- **`Ctrl+N`** (or `:nodes`, v1.10+) — opens a centred picker; ↑/↓ select,
  Enter switches, Esc closes. The active row is marked `●`, the
  default-flagged row `★`.
- **`:context <name>`** — typed switch, same flow under the hood.

The top bar reflects the active profile (`<name> @ <endpoint>`); the
awareness chips (`subs N`, `watch N`, `alerts ●` from v1.10; `notif N`
unread-notification count from v1.15) show what's running in the
background and disappear when there's nothing to surface.

## Theme & accessibility

```toml
[ui]
theme           = "default"   # vibrant green/yellow/red
# theme         = "mono"      # monochrome — same status glyphs, no colour
ascii_fallback  = false       # true → ASCII glyphs (OK / X / ! / > / # / .)
```

Themes are slot-based (Pass / Warn / Fail / Accent / Dim / Info) — adding a
new theme is one file. Glyphs are slot-based too: every component reads
`theme::active().glyphs.X` rather than hardcoding `✓`, so `--ascii` (or
`ascii_fallback = true`) flips every screen at once.

CLI overrides (highest priority):
- `--ascii` — same as `ascii_fallback = true`
- `--no-color` — same as `theme = "mono"`
- `NO_COLOR=1` env honours [no-color.org](https://no-color.org)

Runtime theme switching (`:theme <name>`) lands in v0.6.

## Status

**v1.16.0** on crates.io (May 2026). Fifteen-screen cockpit with drill panes,
54 cockpit verbs (24 also exposed as `--once` CI verbs), simultaneous fleet
view, in-cockpit notification center (toasts + history overlay + optional
desktop / terminal-bell sinks), external Bee log tailing (auto-discovery for
local nodes, or explicit file / command — `[[nodes]].log_file` / `log_command`
/ `--bee-log` / `--bee-log-cmd`), fleet-aggregate webhook, supervised Bee
auto-restart watchdog, ad-hoc fleets from positional node URLs
(`bee-tui url1 url2 …`), `--config <file>`,
fullscreen log mode + inline log filter, node-picker overlay, top-bar
awareness chips, paged help, batch-economics modal, webhook health alerts,
manifest browser, durability + feed timeline + pubsub watches, recursive
uploads, multi-node, theme system, ASCII fallback, vertical scrolling on
every overflow-prone screen, `?` help
overlay, and prebuilt installers for all five major targets.

| Version | Scope | State |
|---|---|---|
| v0.1.0 | S1 Health, S2 Stamps, S10 Command log; single-node; CI; insta tests | ✅ shipped |
| v0.2.0 | S3 SWAP, S4 Lottery, S5 Warmup, S6 Peers, S7 NAT, S8 RPC, S9 Tags, command bar, multi-node, theme system | ✅ shipped |
| v0.9.0 | `:pins-check` / `:loggers` / `:set-logger`, S2 + S6 drill panes, scrollbars, `?` help, `--ascii` / `--no-color`, cargo-dist | ✅ shipped |
| v1.0.0 | Cold-start spinner, footer `?` chips, copy-affordance docs, semver-stable surface | ✅ shipped |
| v1.2.0 | "audit cockpit": S12 Manifests, S13 Watchlist, `:manifest` / `:inspect` / `:durability-check`, utility verbs, log-pane Tab cycling | ✅ shipped |
| v1.3.0 | `--once` CI mode (15 verbs), `:plan-batch` unified topup/dilute decision | ✅ shipped |
| v1.4.0 | Webhook gate alerts, Stamp TTL gate, S3 Market tile (`:price` / `:basefee`), `:check-version`, `:config-doctor`, `:upload-file` | ✅ shipped |
| v1.5.0 | `:upload-collection`, BMT verification on `:durability-check`, `:feed-probe` | ✅ shipped |
| v1.6.0 | S14 Feed Timeline + `:feed-timeline`, `:watch-ref` / `:watch-ref-stop` daemon mode | ✅ shipped |
| v1.7.0 | S15 Pubsub watch + `:pubsub-pss` / `:pubsub-gsoc` / `:pubsub-stop`, swarmscan cross-check | ✅ shipped |
| v1.8.0 | `:grantees-list`, `[pubsub].history_file`, `:pubsub-filter` / `:pubsub-filter-clear` | ✅ shipped |
| v1.9.0 | `[pubsub].rotate_size_mb` + `keep_files` rotation, `:pubsub-replay` | ✅ shipped |
| v1.9.1 | `:context`-switch daemon cleanup + alert-state reset, full-hex continuation lines on S3 SWAP / S4 Lottery / S9 Tags, mdBook catch-up + refreshed VHS GIFs | ✅ shipped |
| v1.10.0 | Node picker overlay (`Ctrl+N` / `:nodes`), top-bar awareness chips (`subs` / `watch` / `alerts`), numeric screen hotkeys, paged help overlay with verb catalogue | ✅ shipped |
| v1.11.0 | S15 Fleet view — simultaneous multi-node health roll-up (3-endpoint probe per node every 10 s, Enter to switch context to the cursored row), `:fleet` verb, `Alt+5` hotkey | ✅ shipped |
| v1.12.0 | Batch-economics modal (`Shift+E`, guided form for topup/dilute/extend/buy/plan previews), `[bee.supervisor].auto_restart` watchdog with exponential backoff + sliding-hour budget + uptime/restart chip, `[fleet].aggregate_webhook_url` for consolidated multi-node alerts | ✅ shipped |
| v1.13.0 | Log-pane viewing polish — `Shift+L` toggles fullscreen log mode (collapses active screen so log pane fills the middle), `/` opens inline case-insensitive substring filter with live match-count chip + Esc-to-clear | ✅ shipped |
| v1.14.0 | Notification center — every gate / fleet transition feeds a top-right toast overlay + `Ctrl+Alt+N` history overlay (last 200, always on). Opt-in desktop notifications (libnotify / D-Bus, pure-Rust zbus backend so no `libdbus` dep) + terminal-bell threshold (`fail` / `warn`). Sits alongside `[alerts]` / `[fleet]` webhooks — same diff pipeline, additional sinks. | ✅ shipped |
| v1.15.0 | External Bee log tailing — fills the bottom pane's Bee-side tabs for a Bee bee-tui didn't spawn. **Local nodes: automatic** — `/proc`-based discovery finds the Bee process and tails its log file / systemd journal / docker logs with zero config (and shows a precise "can't capture, here's the fix" message when Bee logs to a bare terminal). **Remote / explicit:** `log_file` / `--bee-log` tail a file (from EOF), `log_command` / `--bee-log-cmd` tail a command's stdout (`journalctl`, `docker logs`, `ssh … tail`). Per-node; `:context`-switching follows the new node's source. Plus a `notif N` top-bar unread-notification chip, notifications now surface problems already true at startup, and three bug fixes (active-screen key routing, command-bar Enter, notification cold-start). Closes the "connected to a running Bee, log tabs empty" gap. | ✅ shipped |
| v1.15.1 | Config discovery searches `~/.config/bee-tui/` on every platform (macOS / Windows no longer need the platform-native dir); `bee-tui --version` reports the resolved config directory | ✅ shipped |
| v1.15.2 | `--config <file>` CLI flag — points straight at a config file, bypassing the directory search; works in `--once` mode too; fails fast on missing file / unknown extension | ✅ shipped |
| v1.16.0 | Vertical scrolling on seven overflow-prone screens (S1 / S3 / S4 / S7 / S8 free-scroll, S15 / S16 cursor-follow, right-edge scrollbar); S3 Swap two-pane focus (`←→`); fixes cursor-clipping on Fleet + Pubsub. Plus ad-hoc fleets from positional node URLs — `bee-tui url1 url2 …` launches against one or more nodes with no config file | ✅ shipped |

Backed by [bee-rs](https://github.com/ethswarm-tools/bee-rs) v1.6 (full
coverage of the Bee 8.0.0 OpenAPI surface). Full screen specs in
[`docs/PLAN.md`](docs/PLAN.md).

## Stack

- [Ratatui 0.30](https://ratatui.rs/) — terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — terminal backend
- [Tokio](https://tokio.rs/) — async runtime
- [bee-rs ≥ 1.6](https://crates.io/crates/bee-rs) — Bee API client
- 467 lib + insta integration tests cover every gate / status ladder / drill view / scroll
  edge / glyph slot / verb category exhaustiveness / fleet aggregation / supervisor backoff /
  log-pane filter
- MSRV 1.85, `clippy --all-targets -- -D warnings` clean

## Contributing

Issues and PRs welcome at [github.com/ethswarm-tools/bee-tui](https://github.com/ethswarm-tools/bee-tui).
The `[lib]` + `[[bin]]` layout makes integration tests cheap — every new screen
should ship with insta snapshot tests of its pure `view_for` function. Drill
panes follow the same pattern: pure `compute_*_view` fn + insta tests for
realistic / pathological / empty inputs (see `tests/s2_stamps_drill.rs` and
`tests/s6_peers_drill.rs`).

## License

Dual-licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
