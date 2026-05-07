# bee-tui

[![Crates.io](https://img.shields.io/crates/v/bee-tui.svg)](https://crates.io/crates/bee-tui)
[![CI](https://github.com/ethswarm-tools/bee-tui/workflows/CI/badge.svg)](https://github.com/ethswarm-tools/bee-tui/actions)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

A k9s-style terminal cockpit for [Ethereum Swarm](https://www.ethswarm.org/) Bee
node operators — nine live screens that surface the state Bee's API hides:
bucket collisions, redistribution skip reasons, bin starvation, NAT reality,
and a live HTTP tail so operators trust what they see.

```text
 bee-tui   prod-1 @ http://10.0.1.5:1633   ping 12ms   UTC 14:32:18
 [Health]  Stamps  Swap  Lottery  Peers  Network  Warmup  API  Tags    :cmd · Tab to cycle
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

```sh
cargo install bee-tui
```

`bee-tui` ships as a single static binary. Prebuilt installers (Linux / macOS /
Windows) are coming via `cargo-dist` — track [#installers](https://github.com/ethswarm-tools/bee-tui/issues).

## Quickstart

Point bee-tui at a running Bee node:

```sh
# Default config talks to http://localhost:1633
bee-tui

# Or point at a remote node via env
BEE_TUI_CONFIG=~/.config/bee-tui/config.toml bee-tui
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
theme = "default"        # "default" | "mono"
```

The `@env:VAR` token form keeps Bearer tokens out of the config file. With the
`local` default profile, `bee-tui` works with zero config against a local Bee.

## What you get

Nine operator screens plus an always-on command-log pane:

| Screen | What it answers |
|---|---|
| **S1 Health** | "Why is my node unhealthy?" — 10 gates with WHY tooltips encoding tribal knowledge (e.g. "storageRadius decreases ONLY on the 30-min reserve worker tick"). |
| **S2 Stamps** | "Which batch is about to fail uploads?" — worst-bucket fill bar, immutable-vs-mutable rejection semantics (bee#5334), 5-state status ladder. |
| **S3 Swap / cheques** | Chequebook headroom, per-peer net (received − sent) with `\|net\| > 0.5 BZZ` flagging, last-received cheque table. |
| **S4 Lottery** | "Why am I not earning rewards?" — round timeline, anchor summary (last won / played / selected / frozen with `Δ`), stake card with frozen / unhealthy / insufficient-gas reasoning, on-demand `r`-key rchash benchmark. |
| **S5 Warmup** | "What's Bee actually doing during the 25–60-minute cold start?" — five-step checklist with a depth-stability window. |
| **S6 Peers** | Bin saturation strip (Empty / Starving / Healthy / Over) anchored on bee-go's `SaturationPeers=8` and `OverSaturationPeers=18` constants — surfaces the bin-starvation gap no other tool derives. |
| **S7 Network / NAT** | "Why am I unreachable?" — public-vs-private underlay classification, AutoNAT reachability with stability window (flickers under symmetric NAT). |
| **S8 RPC / API health** | Bee API call stats (p50 / p99 latency, error rate over the last 100 calls), pending operator transactions. |
| **S9 Tags / uploads** | "Where is my upload stuck?" — per-tag lifecycle counters (split → sent → synced) and a TagStatus ladder. |
| **S10 Command log** | Always-visible `bee::http` request tail (lazygit-style). The trust anchor — operators learn the API by watching it. |

## Keys

| | |
|---|---|
| `Tab` | cycle screens |
| `:` | open command bar |
| `q`, `Ctrl+C` | quit |
| `r` (Lottery) | run rchash benchmark |
| `Esc` | cancel command bar |

### Command bar

| | |
|---|---|
| `:health`, `:stamps`, `:swap`, `:lottery`, `:peers`, `:network`, `:warmup`, `:api`, `:tags` | jump to that screen |
| `:context <name>` | switch to another node from `config.nodes` |
| `:diagnose` | export a redacted bundle to `$TMPDIR/bee-tui-diagnostic-<ts>.txt` (paste-ready for support threads — Bearer tokens never captured) |
| `:quit`, `:q` | quit |

## Multi-node

Define multiple `[[nodes]]` in `config.toml`. The default profile loads at
startup; `:context <name>` swaps the active connection without restarting. The
top bar reflects the active profile.

## Theme

`config.toml`:

```toml
[ui]
theme = "default"   # vibrant green/yellow/red
# theme = "mono"    # monochrome — same status glyphs, no colour
```

Themes are slot-based (Pass / Warn / Fail / Accent / Dim / Info) — adding a
new theme is one file. Runtime switching (`:theme <name>`) lands in v0.6.

## Status

**v0.2.0** on crates.io (May 2026). Screens S1–S9 + S10 + command bar +
multi-node profiles + diagnostic bundle + theme foundation.

| Version | Scope | State |
|---|---|---|
| v0.1.0 | S1 Health, S2 Stamps, S10 Command log; single-node; CI; insta tests | ✅ shipped |
| v0.2.0 | S3 SWAP, S4 Lottery, S5 Warmup, S6 Peers, S7 NAT, S8 RPC, S9 Tags, `:command` bar, `:context`, `:diagnose`, top bar, theme system | ✅ shipped |
| v0.9 | Polish, mdBook docs, VHS demos, cargo-dist installers, beta program | 🔜 next |
| v1.0 | Stable, awesome-swarm PR, blog post | release-only |

Backed by [bee-rs](https://github.com/ethswarm-tools/bee-rs) v1.4. Full screen
specs in [`docs/PLAN.md`](docs/PLAN.md).

## Stack

- [Ratatui 0.30](https://ratatui.rs/) — terminal UI framework
- [crossterm](https://github.com/crossterm-rs/crossterm) — terminal backend
- [Tokio](https://tokio.rs/) — async runtime
- [bee-rs ≥ 1.4](https://crates.io/crates/bee-rs) — Bee API client
- 136 insta + unit tests cover every gate / status ladder / view rendering edge
- MSRV 1.85, `clippy --all-targets -- -D warnings` clean

## Contributing

Issues and PRs welcome at [github.com/ethswarm-tools/bee-tui](https://github.com/ethswarm-tools/bee-tui).
The `[lib]` + `[[bin]]` layout makes integration tests cheap — every new screen
should ship with insta snapshot tests of its pure `view_for` function.

## License

Dual-licensed under either [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
