# Changelog

All notable changes to bee-tui will be documented in this file. The
format follows [Keep a Changelog]; the project adheres to
[Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

### Added

- **Spawn Bee from bee-tui (increment 1 of 4).** When `[bee].bin`
  + `[bee].config` are set in `config.toml` (or via `--bee-bin` /
  `--bee-config` CLI flags), bee-tui now launches Bee as a child
  process before opening the cockpit. Bee's stdout + stderr are
  captured to a temp file (`$TMPDIR/bee-tui-spawned-<ts>.log`)
  ready for the bottom-pane log tail (next increment); /health
  is polled until 200 OK before the TUI opens; SIGTERM-pgroup +
  5s grace + SIGKILL fallback on quit. If Bee crashes
  mid-session, a red "bee exited (code N)" chip appears in the
  top bar — no auto-restart, the operator decides what to do.
  Legacy "connect to a running Bee" mode unchanged when `[bee]`
  is unset.
- **Shift+Tab cycles screens backward.** The README and in-app
  `?` overlay had advertised this for a while; only `Tab` was
  actually wired. crossterm surfaces Shift+Tab as
  `KeyCode::BackTab` (a separate variant rather than Tab + a
  modifier) — both branches are now handled.

### Changed

- **`q` now requires a double-tap to quit.** First `q` shows a
  footer hint ("press q again to quit (Esc cancels)"); a second
  `q` within ~1.5 s commits. Any other key (or `Esc`) cancels
  the pending quit. Operators routinely leave the cockpit
  running in a background pane and an accidental `q` cost them
  the session — this guards against that without changing the
  shape of the action. `Ctrl+C` / `Ctrl+D` and `:q` remain
  unguarded as immediate-quit escape hatches.

## [1.0.0] - 2026-05-07

First stable release. The full nine-screen operator cockpit
plus drill panes, command bar, multi-node, theme system,
ASCII fallback, scrollbars, `?` help overlay, and prebuilt
installers — committed surface, semver discipline from here on.

### Added

- **Cold-start spinner.** Every `loading…` line now leads with
  a tick-driven spinner glyph (10-frame braille for Unicode,
  4-frame `|/-\` for ASCII) so the first few seconds feel
  alive instead of stuck. Single process-wide AtomicUsize
  advanced once per `Action::Tick` from
  `App::handle_actions`; honours `--ascii`.
- **`?` footer chip on every screen.** The `?` overlay was
  reachable from the top tab strip's hint already; this adds
  matching ` ? help ` chip to every screen's bottom-bar
  keymap so operators reading from there discover it too.
- **README copy-affordance note.** Documents that mouse mode
  is off by default so terminal-native click-drag selection
  works for copying peer overlays / batch IDs / cheque hashes
  out of the cockpit.

### Notes

- **Backwards compatibility commitment.** Public crate-level
  surface (`bee_tui::components::*::view_for` /
  `compute_*_view` pure functions, `bee_tui::watch::*`
  snapshot shapes, CLI flags, `[ui]` config schema) is now
  semver-stable. Breaking changes go to v2. Internal
  refactors (component fields, render helpers) are not part
  of the contract.
- **bee-rs dependency**: 1.6 (full Bee 8.0.0 OpenAPI
  coverage). bee-rs has its own semver track.
- **Test count**: 107 lib + ~75 insta integration tests.
  cargo clippy --all-targets --all-features clean. Binary
  2.1 MiB stripped on linux x86_64.
- **Distribution**: cargo-dist autobuilds binaries for five
  targets (mac arm64/x86_64, linux arm64/x86_64, windows
  x86_64) on every tag push, plus `curl|sh` / `irm|iex`
  one-line installers.

## [0.9.0] - 2026-05-07

The 1.0 candidate. Phases A/B/C of the 1.0 push complete; this
release is what goes out for operator beta. v1.0.0 follows in
~2 weeks once beta feedback is incorporated.

### Added

- **bee-rs 1.6 fix wired**: `:set-logger <expr> <level>` calls
  `set_logger(expr, verbosity)`. The 1.5-and-earlier
  `set_logger_verbosity` was provably broken — emitted
  `PUT /loggers/{exp}` against Bee's `PUT /loggers/{exp}/{verb}`
  route. Live-validated on Bee 2.7.2 toggling node/api between
  debug and info.
- **`:pins-check`** — walks every pinned root via
  bee-rs 1.5's `check_pins(None)`, writes results to
  `$TMPDIR/bee-tui-pins-check-<profile>-<ts>.txt` in
  tail-friendly format (`<ref>  total=N  missing=N
  invalid=N  healthy|UNHEALTHY`). Per-profile filename so two
  parallel invocations against different `:context` endpoints
  don't collide.
- **`:loggers`** — snapshots `/loggers` to a profile-tagged
  file in `$TMPDIR`, sorted loudest-first (all/trace/debug
  ahead of info/warning/error/silent).
- **`:set-logger <expr> <level>`** — bumps a Bee subsystem
  to a verbosity from inside the cockpit. Levels validated
  client-side against Bee's enum.
- **S2 Stamps drill** (`↵` on a batch row) — fetches
  `/stamps/{id}/buckets` and renders a fill-percentage
  histogram across six bins (0%, 1–19%, …, 100%) plus the
  top 10 worst buckets. Two batches with identical `utilization`
  can fail uploads under wildly different conditions; the drill
  answers *how concentrated* the load is. `compute_drill_view`
  is pure — three insta snapshot tests pin realistic /
  pathological / empty inputs.
- **S6 Peers drill** (`↵` on a peer row) — parallel fetch via
  `tokio::join!` of `peer_balance` + `peer_cheques` +
  `peer_settlement` + `ping_peer`. Each field fails
  independently so a 404 on `/chequebook/cheque/{peer}`
  (peers we've never exchanged cheques with) doesn't blank
  the drill — just shows `error: 404` for that field. Late
  results from cancelled drills are dropped silently. Three
  insta snapshot tests pin realistic / partial-failure /
  all-failed inputs.
- **Per-screen `?` help overlay.** Centred floating panel
  with a per-screen keymap pulled from `screen_keymap()`
  plus four global rows (Tab / `?` / `:` / `q`). `?` toggles,
  Esc / `?` / `q` dismiss. Modal dispatch swallows the open
  *and* close key presses so they don't propagate to the
  active screen — Esc on the overlay doesn't collapse a
  drill underneath.
- **Scrollbars on long lists** (S2 / S6 / S9). Pinned column
  header + scrollable body via the new `components/scroll.rs`
  helper (`clamp_scroll` + `render_scrollbar`). S6 selection
  drives scroll; S2 maps row→line index so continuation
  tooltips count as visual lines but the cursor snaps to the
  row's main line; S9 has j/k/PgUp/PgDn/Home scroll without
  a cursor since rows are non-selectable.
- **Glyph slot system.** New `theme::Glyphs::unicode/ascii`
  with twelve slots (pass / warn / fail / pending /
  in_progress / bar_filled / bar_empty / cursor / ellipsis /
  continuation / bullet / em_dash). Components read
  `theme::active().glyphs.X` rather than hardcoding `✓`.
- **`--ascii` flag** — ASCII glyphs for terminals with poor
  Unicode support (Windows Terminal pre-Win11, screen
  readers, some SSH chains). Equivalent to
  `[ui].ascii_fallback = true` in config.
- **`--no-color` flag + `NO_COLOR` env honour** —
  <https://no-color.org> compliant. Either signal forces the
  mono palette regardless of `[ui].theme`.
- **S3 Swap header now shows the chequebook contract address**
  (bee-rs 1.5's `chequebook_address`). Lets operators paste
  the contract straight into a block explorer without
  unpacking `/wallet`.

### Changed

- **bee-rs dependency bumped 1.4 → 1.6.** 1.5 closed the last
  Bee 8.0.0 OpenAPI gaps (`chequebook_address`, `check_pins`).
  1.6 fixed the broken `set_logger_verbosity`. Both releases
  shipped to crates.io + GitHub.
- **`Stamps::new` and `Peers::new` take `Arc<ApiClient>`** so
  the drill can spawn its own fetches. `build_screens` updated
  to pass it.
- **S9 footer mentions scroll keys** (`jk/PgUp/PgDn`).
- **Top tab strip hint** extended to `:cmd · Tab · ? help`
  for ? discoverability.

### Polish & tooling

- **cargo-dist scaffold** — `dist-workspace.toml` +
  autogenerated `.github/workflows/release.yml`. Tag push
  triggers builds for five targets (mac arm64/x86_64, linux
  arm64/x86_64, windows x86_64) producing tarballs + shell +
  powershell installers + per-tarball sha256.
- **mdBook scaffold** at `docs/book/` with 19 stub pages —
  the structure is in place, content fills in driven by
  beta feedback.
- **VHS tape templates** at `docs/tapes/` — cold-start, S2
  drill, S6 drill, `:pins-check`. Render to GIFs for the
  README + 1.0 blog post.
- **R3 audit report** at `docs/R3-observations.md` — pre-1.0
  code-level + targeted-live-probe audit. No red findings.

### Tests

106 lib tests + ~75 insta integration tests — every drill
view, scroll edge, glyph slot, ladder transition, and
fan-out partial-failure shape pinned. `cargo clippy
--all-targets -- -D warnings` clean. Binary 2.1 MiB stripped
on linux x86_64.

## [0.2.0] - 2026-05-07

A massive jump from v0.1.0's three-screen reservation: nine operator
screens, a k9s-style command bar, multi-node profile switching, and
the theme system foundation. The full breakdown:

### Added

- **S9 Tags / uploads** — ninth operator screen, the answer to
  "where is my upload stuck?". Each row surfaces every Bee tag's
  lifecycle counters (total / split / seen / stored / sent / synced)
  with a derived `TagStatus` ladder naming the active phase
  (Pending / Splitting / Pushing / Syncing / Synced). Sort is
  newest-first by UID descending, so a fresh upload appears at the
  top. The header aggregates split / sent / synced across every tag
  plus an `active` count of non-Pending non-Synced rows. Driven by a
  new 5 s `/tags` poller. 9 insta snapshot tests pin every status
  transition + the table-level invariants.
- **Theme system foundation.** Slot-based palette
  (`Pass / Warn / Fail / Header / Accent / Dim / …`) with `default`
  and `mono` variants out of the box. Themes encode *intent*, not
  literal `Color` values, so future variants are one-file changes
  instead of many-file refactors. Configured via `[ui] theme` in
  `config.toml`; unknown names fall back to the default palette with
  a tracing warning. Active theme is held in a `OnceLock`; runtime
  switching (`:theme <name>`) is left for v0.6.
  - Migrated this release: App's top bar, command-bar prompt + status
    line, S1 Health. Other screens still use hard-coded literals
    pending follow-up — behaviour is identical until those flip.
- **`[ui]` config section.** `theme = "default" | "mono"` and
  `ascii_fallback = false` (reserved for follow-up; not yet wired).
- **Tab now cycles nine screens** Health → Stamps → Swap → Lottery →
  Peers → Network → Warmup → API → Tags. `:tags` jumps directly via
  the `:command` bar.

- **k9s-style `:command` bar.** `:` opens an input line at the bottom
  of the screen; Backspace edits, Enter dispatches, Esc cancels.
  Component-level key dispatch is suppressed while the bar is open,
  so typing `r` inside `:diagnose` doesn't fire Lottery's rchash
  benchmark behind the prompt.
- **Direct screen jumps.** `:health`, `:stamps`, `:swap`, `:lottery`,
  `:peers`, `:network`, `:warmup`, `:api` jump straight to the named
  tab. Case-insensitive against `SCREEN_NAMES`. Unknown commands
  surface a one-line error in the status row.
- **`:diagnose` (alias `:diag`).** Writes a redacted, paste-ready
  bundle (profile + endpoint + every health gate's status + last
  50 captured Bee API calls) to
  `$TMPDIR/bee-tui-diagnostic-<unixtime>.txt`. URLs are reduced to
  their path component before being written; Bearer tokens, if any,
  live in headers and aren't captured.
- **`:context <name>` (alias `:ctx`).** Multi-node switcher — drops
  the current `BeeWatch` hub and respawns against another `Config`
  entry. Component-internal state is intentionally lost since a
  profile switch is a fresh slate. `:context` with no argument lists
  the configured node names.
- **Top bar.** Replaces the bare "Tab to switch" hint with a richer
  metadata line — `bee-tui` badge, active profile name, endpoint URL,
  live ping value off the existing health stream, UTC HH:MM:SS clock.
  Tab strip stays on row 2.
- **`:quit` / `:q`.** Routes through the existing Action::Quit
  pipeline so the operator can exit without leaving the keyboard
  home row.

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
