# bee-tui

[![CI](https://github.com/ethswarm-tools/bee-tui/workflows/CI/badge.svg)](https://github.com/ethswarm-tools/bee-tui/actions)
[![License: MIT OR Apache-2.0](https://img.shields.io/badge/license-MIT%20OR%20Apache--2.0-blue.svg)](LICENSE-MIT)

Production-grade k9s-style terminal cockpit for Ethereum Swarm Bee node operators.

> **Status: v0.1 in flight.** S1 Health and S10 Command-log shipped;
> S2 Stamps next. Crate name reserved at
> [`bee-tui v0.0.1`](https://crates.io/crates/bee-tui/0.0.1) on crates.io.
> Plan canonical at [`docs/PLAN.md`](docs/PLAN.md). Current session pin
> in [`docs/STATUS.md`](docs/STATUS.md).

## What this is

bee-tui is the operator's truth-teller — a live terminal cockpit that surfaces
the state Bee's API hides:

- Bucket collisions before `utilization` says 100%
- Redistribution skip reasons (`LastPlayedRound` is misleading)
- Health gates with WHY (storageRadius vs committedDepth, 30-min wakeup tick)
- NAT reality (libp2p AutoNAT under symmetric NAT)
- HTTP request log so operators trust what's happening

Built on top of [bee-rs](https://github.com/ethswarm-tools/bee-rs), the Rust
client for Bee.

## Status

| Screen | State |
|---|---|
| S1 Health gates | ✅ shipped (9 of 10 gates; bin saturation deferred) |
| S2 Stamps (volume+duration UX, bucket histogram) | 🔜 next chunk |
| S10 Command-log pane (`bee::http` tracing tail) | ✅ shipped |
| S3-S9 | ⏳ later milestones — see [docs/PLAN.md § 12](docs/PLAN.md) |

The plan was validated against:

- ~50 GitHub issues across `ethersphere/bee`, `swarm-cli`, `bee-dashboard` for
  operator pain points
- Bee node source for stamp / redistribution / accounting / sync internals
- Decentralized-storage TUI prior art (lntop, ipfs-tui, k9s)
- 2026 Ratatui ecosystem state-of-the-art

See [`docs/research/`](docs/research/) for the eight research streams.

## Roadmap

| Version | Scope |
|---|---|
| v0.1 | S1 Health gates, S2 Stamps, S10 Command log; single-node |
| v0.2 | S3 SWAP, S4 Lottery + rchash benchmark |
| v0.3 | S5 Warmup, S6 Peers, S7 NAT, S8 RPC |
| v0.4 | Multi-node, `:context` switcher, diagnostic bundle |
| v0.5 | S9 Uploads (chunks/stream WS), theme system |
| v1.0 | Polish, mdBook docs, awesome-swarm PR |

Full roadmap and screen specs in [`docs/PLAN.md`](docs/PLAN.md).

## Stack

- [Ratatui 0.30](https://ratatui.rs/) — terminal UI framework
- [crossterm 0.29](https://github.com/crossterm-rs/crossterm) — terminal backend
- [Tokio](https://tokio.rs/) — async runtime
- [bee-rs ≥ 1.3.0](https://crates.io/crates/bee-rs) — Bee API client
- Single static binary via `cargo install bee-tui` (when published)

## License

Dual-licensed under either of [Apache-2.0](LICENSE-APACHE) or [MIT](LICENSE-MIT)
at your option.
