# Changelog

All notable changes to bee-tui will be documented in this file. The
format follows [Keep a Changelog]; the project adheres to
[Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

### Added

- **S8 RPC / API health** — eighth operator screen, closing out
  v0.3 from `docs/PLAN.md` § 12. PLAN's framing was Gnosis-RPC
  latency + remote block height; Bee doesn't expose either, so we
  pivot to what we *can* measure:
  - **Bee API call stats** (p50 / p99 latency + error rate) computed
    over the last 100 captured tracing events from the same source
    that drives S10's command-log. Bee API latency is the more
    operator-relevant metric anyway — a slow API surface means a
    sluggish local node regardless of the underlying RPC.
  - **Chain state** — `block` / `chain_tip` / their delta from
    `/chainstate`.
  - **Pending operator transactions** from `/transactions` with hash
    short, nonce, creation timestamp, and operator description so a
    stuck postage-topup or stake-deposit doesn't disappear.
  - The "Bee doesn't expose its eth RPC URL or remote chain tip"
    gap is acknowledged inline, with a pointer to external monitoring
    tools instead of pretending we have full RPC visibility.
- **Tab now cycles eight screens** Health → Stamps → Swap → Lottery →
  Peers → Network → Warmup → API.
- 5 insta snapshot tests pin the percentile math (empty / all-success
  / mixed errors / pending tx rows / chain lagging).

- **S5 Warmup** — seventh operator screen. Answers bee#4746 (the
  25–60 minute cold-start opacity where Bee bootstraps internally
  and the operator sees nothing actionable). Reuses the existing
  health / stamps / topology streams (no new poller) and renders an
  elapsed counter plus a five-step checklist:
  - Postage snapshot loaded
  - Peer bootstrap (against a heuristic 50-peer target)
  - Kademlia depth stable (5-tick observation window)
  - Reserve fill (`reserve_size_within_radius / 65_536`)
  - Stabilization (terminal step keyed on `is_warming_up=false`)
  Elapsed timer captures the first `is_warming_up=true` observation
  and freezes the moment Bee flips it back to false. Screen stays
  useful post-warmup as a "definition of done" view.
- **Tab now cycles seven screens** Health → Stamps → Swap → Lottery →
  Peers → Network → Warmup.
- 5 insta snapshot tests pin the StepState transitions across every
  phase (no-data → fresh → mid → almost-done → complete).

- **S7 Network / NAT** — sixth operator screen and the
  highest-leverage answer to bee#4194 ("I have peers but I'm
  unreachable"). Driven by a new 60 s `/addresses` poller plus the
  existing 5 s topology stream. Three panes:
  - **Identity** — short overlay + ethereum address.
  - **Connections + reachability** — inbound vs outbound counts
    derived from `MetricSnapshotView::session_connection_direction`,
    plus the AutoNAT `reachability` and `networkAvailability` strings
    with a *stability window* ("stable for 9m") computed in the
    component. Symmetric NAT makes `isReachable` flicker; the window
    converts the flap into observable signal.
  - **Public addresses** — every underlay multiaddr classified
    Public / Private / Unknown by parsing the `/ip4` or `/ip6`
    segment. RFC 1918, loopback, link-local, ULA, and `fe80::/10`
    count as Private; DNS multiaddrs surface as Unknown so the
    screen doesn't pretend to know without resolving.
  - PLAN's external port-check + relay candidate enumeration require
    services Bee doesn't expose; the screen documents that gap inline
    and points to `nmap` from a separate machine instead of faking it.
- **Tab now cycles six screens** Health → Stamps → Swap → Lottery →
  Peers → Network.
- 6 insta snapshot tests pin the reachability ladder and underlay
  classification.

- **S6 Peers + bin saturation** — fifth operator screen and the
  headline answer to the bin-starvation visibility gap (no other tool
  in the ecosystem derives this). Driven by a new 5 s `/topology`
  poller backed by the per-bin `BinInfo` data from bee-rs 1.4. Two
  panes:
  - **Bin saturation strip** with a four-state ladder
    (Empty / Starving / Healthy / Over) anchored on the bee-go
    `SaturationPeers=8` and `OverSaturationPeers=18` constants. Bins
    more than four positions past the kademlia depth are expected to
    be sparse and don't trigger Starving — operators only see alarms
    for bins that should be saturated.
  - **Peer table** flattening every bin's `connectedPeers` into a
    stable list (sort by bin asc, overlay asc) with bin / overlay
    short / direction / latency (EWMA ns → ms) / healthy / per-peer
    reachability columns.
- **S1 bin saturation gate** is no longer a placeholder. Reads the
  same `/topology` stream and Pass/Warn/Unknown classifies based on
  whether every bin at or below the kademlia depth has ≥ 8 connected
  peers. The `value` line lists up to five starving bin numbers
  inline so the operator can see exactly which bins to fill.
- **Tab now cycles five screens** Health → Stamps → Swap → Lottery →
  Peers.
- 6 insta snapshot tests pin every Peers view variant; 2 new S1
  snapshot tests pin the Pass and Warn bin-saturation cases.
- **bee-rs dependency bumped to 1.4** (just published) for the
  extended `Topology` parse — full per-bin `BinInfo` map,
  `reachability` + `networkAvailability` strings, light-node bin,
  and `MetricSnapshotView` per peer.

- **S4 Lottery / redistribution** — fourth operator screen, the
  highest-leverage answer to "why am I not earning rewards?"
  (bee#4849). Three panes driven by the existing 2 s
  redistribution-state stream and a new 30 s `/stake` poller:
  - **Round timeline** with commit / reveal / claim segments (block
    boundaries hard-coded from `pkg/storageincentives/agent.go` —
    152 blocks/round, 38 per on-chain phase) and a 24-cell
    block-of-round progress bar.
  - **Anchor summary** — last won / played / selected / frozen
    rounds, each with a human "Δ" string ("4 rounds ago", "never",
    "this round") so operators read the cadence at a glance.
  - **Stake card** with a six-state ladder reasoning tree (Healthy /
    Unstaked / Frozen / InsufficientGas / Unhealthy / Unknown) that
    reduces the four scattered RedistributionState booleans
    (is_frozen, is_healthy, has_sufficient_funds, is_fully_synced)
    plus staked amount to a single tooltip.
  - PLAN's "last 20 rounds with skip reasons" view requires an
    upstream `RoundData[]` port in bee-rs and is deferred; the
    anchor summary covers the same question with today's API.
  - **`r` runs an on-demand rchash benchmark.** Fires
    `/rchash/{depth}/{anchor1}/{anchor2}` (depth = node's current
    `storage_radius`, anchors deterministic so repeat measurements
    compare cleanly) and renders the duration vs the 95 s
    commit-window deadline — green when safe, red when over budget.
    Lifecycle owned by an internal mpsc inside the Lottery component
    rather than a global Action variant.
- **Tab now cycles four screens** Health → Stamps → Swap → Lottery.
- 10 insta snapshot tests pin every Lottery view variant.
- **S3 SWAP / cheques** — third operator screen. Three stacked panes
  driven by a new 30 s `SwapSnapshot` poller (`/chequebook/balance`,
  `/chequebook/cheque`, `/settlements`, `/timesettlements`):
  - **Chequebook card** with `available / total` headroom — flagged
    Tight when uncashed debt eats >80 % of total BZZ.
  - **Last received cheques** per peer, sorted by payout descending;
    peers that have never sent us a cheque sink to the bottom but
    stay visible because absence is itself useful signal.
  - **Per-peer settlements** sorted by `|received - sent|` descending
    so the most out-of-balance peer floats to the top, with `|net|
    > 0.5 BZZ` rows highlighted red — that's where cashout pressure
    builds up first.
  - PLUR amounts render as `BZZ x.xxxx` (4 decimals) with explicit
    `+/-` signs on net so positive / negative read at a glance.
- **Tab now cycles three screens** Health → Stamps → Swap → Health.
- 7 insta snapshot tests pin every Swap view variant.

## [0.1.0] - 2026-05-07

### Added

- **S1 Health gates** — first operator-facing screen. Polls
  `/status`, `/chainstate`, `/wallet`, and `/redistributionstate`
  every 2 seconds and renders ten gates with a tri-state status
  ladder (Pass / Warn / Fail / Unknown). Each gate carries a
  `value` and an optional `why` continuation that encodes tribal
  knowledge from GitHub-issue research — most notably the
  `storageRadius < committedDepth` case with the
  "decreases ONLY on the 30-min reserve worker tick" tooltip
  (bee#5428). Bin-saturation gate is a `v0.2` placeholder.
- **S2 Stamps** — postage batch table with the volume + duration
  framing the community is moving toward (bee#4992 retiring
  depth+amount). Worst-bucket fill bar and raw `utilization /
  BucketUpperBound` count tell the truth that Bee's `utilization`
  field is `MaxBucketCount` — operators see *which* batch is
  about to fail uploads even when average usage is far from
  100 %. Five-state status ladder (Pending / Healthy / Skewed /
  Critical / Expired) with split tooltips for immutable
  vs mutable critical batches (bee#5334).
- **S10 Command-log pane** — lazygit-style append-only tail of
  `bee::http` events (`method`, `url`, `status`, `elapsed_ms`)
  rendered in an 8-line strip at the bottom of the screen.
  Captured via a custom `tracing-subscriber::Layer` so every
  Bee API call placed by `bee-rs` shows up live, with no extra
  instrumentation in the cockpit.
- **Tab cycles screens.** `Tab` switches Health ↔ Stamps; the
  top-bar tab strip highlights the active screen. v0.4 will
  replace this with a k9s-style `:command` switcher.
- **k9s-style watch / informer architecture.** `BeeWatch` spawns
  one polling task per resource group, publishing snapshots via
  `tokio::sync::watch`. Cancellation is hierarchical: each
  poller's `CancellationToken` is a child of the hub's, which is
  itself a child of the App's root. Quitting cancels everything in
  one go.
- **`ApiClient` wrapper** around `bee::Client` + `NodeConfig`.
  Resolves the `@env:VAR` token indirection and routes through
  `bee::Client::with_token` / `bee::Client::new` accordingly.
- **Multi-node-ready config schema.** `Config::nodes` is a list of
  `NodeConfig { name, url, token, default }`; default profile is
  a single `local` entry pointing at `http://localhost:1633`.
  Multi-node UX itself ships in v0.4.
- **Library + binary layout.** `[lib]` and `[[bin]]` coexist so
  integration tests in `tests/` can reach internals like
  `Health::gates_for` and `Stamps::rows_for` without launching the
  full TUI loop. 11 insta snapshot tests pin every gate and stamp
  row variant.

## [0.0.1] - 2026-05-07

### Added

- **Initial crates.io reservation publish.** Scaffolds the bee-tui
  binary from the upstream
  [ratatui/templates component](https://github.com/ratatui/templates)
  template. Builds, runs, and prints a placeholder home screen — no
  bee-rs integration or operator screens yet. The reservation
  protects the `bee-tui` crate name while the implementation work
  outlined in [`docs/PLAN.md`](docs/PLAN.md) lands.

### Notes

- This release is **not functional** for Bee operators. Watch the
  repository for `0.1.0`, which lands the first three screens
  (S1 Health gates, S2 Stamps, S10 Command log).
