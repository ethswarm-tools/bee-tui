# bee-tui

[![Crates.io](https://img.shields.io/crates/v/bee-tui.svg)](https://crates.io/crates/bee-tui)
[![CI](https://github.com/ethswarm-tools/bee-tui/workflows/CI/badge.svg)](https://github.com/ethswarm-tools/bee-tui/actions)
[![Docs](https://img.shields.io/badge/docs-mdBook-blue)](https://ethswarm-tools.github.io/bee-tui/)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

> **Operator handbook**: <https://ethswarm-tools.github.io/bee-tui/> — full
> per-screen reference, command bar, keymap, FAQ.

A k9s-style terminal cockpit for [Ethereum Swarm](https://www.ethswarm.org/) Bee
node operators — fourteen live screens that surface the state Bee's API hides:
bucket collisions, redistribution skip reasons, bin starvation, NAT reality,
manifest contents, durability checks, feed timelines, pubsub watches — plus
a live HTTP tail so operators trust what they see.

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

# Or point at a remote node via env
BEE_TUI_CONFIG=~/.config/bee-tui/config.toml bee-tui

# Force ASCII glyphs (Windows Terminal pre-Win11, screen readers, broken SSH chains)
bee-tui --ascii

# Suppress colour (also honours NO_COLOR env per <https://no-color.org>)
bee-tui --no-color
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
url  = "http://localhost:1633"

[ui]
theme           = "default"        # "default" | "mono"
ascii_fallback  = false            # true = same as `--ascii`
```

The `@env:VAR` token form keeps Bearer tokens out of the config file. With the
`local` default profile, `bee-tui` works with zero config against a local Bee.

## What you get

Nine operator screens plus an always-on command-log pane:

| Screen | What it answers |
|---|---|
| **S1 Health** | "Why is my node unhealthy?" — 10 gates with WHY tooltips encoding tribal knowledge (e.g. "storageRadius decreases ONLY on the 30-min reserve worker tick"). |
| **S2 Stamps** | "Which batch is about to fail uploads?" — worst-bucket fill bar, immutable-vs-mutable rejection semantics (bee#5334), 5-state status ladder. **`↵` drills into the per-bucket fill histogram.** |
| **S3 Swap / cheques** | Chequebook headroom (with on-chain contract address), per-peer net (received − sent) with `\|net\| > 0.5 BZZ` flagging, last-received cheque table. |
| **S4 Lottery** | "Why am I not earning rewards?" — round timeline, anchor summary (last won / played / selected / frozen with `Δ`), stake card with frozen / unhealthy / insufficient-gas reasoning, on-demand `r`-key rchash benchmark. |
| **S5 Warmup** | "What's Bee actually doing during the 25–60-minute cold start?" — five-step checklist with a depth-stability window. |
| **S6 Peers** | Bin saturation strip (Empty / Starving / Healthy / Over) anchored on bee-go's `SaturationPeers=8` and `OverSaturationPeers=18` constants — surfaces the bin-starvation gap no other tool derives. **`↵` drills into per-peer balance / cheques / settlement / ping.** |
| **S7 Network / NAT** | "Why am I unreachable?" — public-vs-private underlay classification, AutoNAT reachability with stability window (flickers under symmetric NAT). |
| **S8 RPC / API health** | Bee API call stats (p50 / p99 latency, error rate over the last 100 calls), pending operator transactions. |
| **S9 Tags / uploads** | "Where is my upload stuck?" — per-tag lifecycle counters (split → sent → synced) and a TagStatus ladder. Long lists scroll with `j k` / `PgUp PgDn` / `Home`. |
| **S10 Command log** | Always-visible `bee::http` request tail (lazygit-style). The trust anchor — operators learn the API by watching it. |

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

## Keys

| | |
|---|---|
| `Tab` / `Shift+Tab` | cycle screens forward / backward |
| `?` | toggle per-screen help overlay |
| `:` | open command bar |
| `qq` | quit (double-tap within ~1.5s; `:q` also works) |
| `Ctrl+C` / `Ctrl+D` | quit immediately (escape hatch) |
| `↑↓` / `j k` | move selection (S2, S6) or scroll (S9) |
| `↵` | drill selected row (S2, S6) |
| `Esc` | close drill / overlay |
| `r` | run rchash benchmark (S4 Lottery) |
| `PgUp` / `PgDn` / `Home` | page through long lists (S9) |

`?` is the source of truth for screen-specific keymaps — every screen advertises
its keys in the overlay.

**Copying values out of the cockpit:** mouse mode is off by default, so your
terminal's native selection works. Click-drag a peer overlay or batch ID,
then paste it into a block explorer / Discord / etc. — bee-tui doesn't
intercept it.

### Command bar

| | |
|---|---|
| `:health`, `:stamps`, `:swap`, `:lottery`, `:peers`, `:network`, `:warmup`, `:api`, `:tags` | jump to that screen |
| `:context <name>` | switch to another node from `config.nodes` |
| `:diagnose` | export a redacted bundle to `$TMPDIR/bee-tui-diagnostic-<ts>.txt` (paste-ready for support threads — Bearer tokens never captured) |
| `:pins-check` | walk every pinned root via `/pins/check`, write results to `$TMPDIR/bee-tui-pins-check-<profile>-<ts>.txt` (NDJSON streamed by Bee, collected and tail-friendly) — see [demo](docs/tapes/pins-check.gif) |
| `:loggers` | snapshot `/loggers` to `$TMPDIR/bee-tui-loggers-<profile>-<ts>.txt`, sorted loudest-first |
| `:set-logger <expr> <level>` | call `PUT /loggers/{exp}/{level}` — bump a Bee subsystem to `debug` / `info` / etc. without curl |
| `:quit`, `:q` | quit |

## Multi-node

Define multiple `[[nodes]]` in `config.toml`. The default profile loads at
startup; `:context <name>` swaps the active connection without restarting. The
top bar reflects the active profile.

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

**v1.9.0** on crates.io (May 2026). Fourteen-screen cockpit with drill panes,
51 cockpit verbs (24 also exposed as `--once` CI verbs), webhook health alerts,
manifest browser, durability + feed timeline + pubsub watches, recursive uploads,
multi-node, theme system, ASCII fallback, scrollbars, `?` help overlay, and
prebuilt installers for all five major targets.

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

Backed by [bee-rs](https://github.com/ethswarm-tools/bee-rs) v1.6 (full
coverage of the Bee 8.0.0 OpenAPI surface). Full screen specs in
[`docs/PLAN.md`](docs/PLAN.md).

## Stack

- [Ratatui 0.30](https://ratatui.rs/) — terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — terminal backend
- [Tokio](https://tokio.rs/) — async runtime
- [bee-rs ≥ 1.6](https://crates.io/crates/bee-rs) — Bee API client
- 106 lib + insta tests cover every gate / status ladder / drill view / scroll
  edge / glyph slot
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
