# Changelog

All notable changes to bee-tui will be documented in this file. The
format follows [Keep a Changelog]; the project adheres to
[Semantic Versioning].

[Keep a Changelog]: https://keepachangelog.com/en/1.1.0/
[Semantic Versioning]: https://semver.org/spec/v2.0.0.html

## [Unreleased]

TBD.

## [1.12.0] - 2026-05-13

The "operator polish" minor release. Three additive features
that each close a real workflow pain point:

1. **Batch-economics modal** turns the most-used numeric workflow
   (predict-the-cost-of-X) from "type a six-arg verb line" into
   "press `Shift+E` and answer four prompts."
2. **Supervised Bee auto-restart watchdog** with exponential
   backoff + sliding-hour budget closes the v1.x gap where bee-tui
   would log a crash and go dim instead of relaunching Bee.
3. **Fleet-aggregate webhook** consolidates per-node alerts across
   the S15 fleet into a single rolled-up POST — operators running
   5+ nodes stop getting five Slack pings per network blip.

### Added

- **Batch-economics modal** (`Shift+E` from anywhere) — opens a
  centred overlay that walks the operator through action choice
  (`t/d/e/b/p` for topup / dilute / extend / buy / plan-batch),
  field-by-field entry of the verb's positional args, and
  shows the preview output inline. The modal reuses the existing
  `run_*_preview` methods on `App` so there's zero code
  duplication — the modal just assembles the verb line and
  dispatches. Help-overlay row added (`E` row in the Keys page).
- **`[bee.supervisor]` config section** — when
  `auto_restart = true` (default `false`), bee-tui watches the
  supervised Bee child via the existing `BeeSupervisor` and
  re-spawns it on exit. Exponential backoff
  (`backoff_initial_secs` doubling up to `backoff_max_secs`, both
  configurable) protects against fast crashloops; a sliding
  one-hour `max_restarts_per_hour` budget (default 6) stops the
  watchdog cold when something is fundamentally broken. The
  top-bar Bee chip now shows `bee running 4d3h (2 restarts)` when
  the watchdog is on (with green background while running) and
  `bee: max restarts (6/6) hit` when the budget is exhausted.
  Without the watchdog the v1.11 chip behaviour (red-on-crash
  only) is preserved.
- **`[fleet]` config section** with
  `aggregate_webhook_url` + `aggregate_window_secs` (default
  60s) — on every fleet-poll tick bee-tui ingests the new
  snapshot, notes any nodes that transitioned to a worse or
  recovered status, and buffers them. After the coalesce window
  elapses, one consolidated POST goes out with a body like
  `Fleet alert: 2 fail · 1 warn …`. Steady-state failures don't
  re-alert; `Unknown` cold-start transitions are suppressed.
  Per-node `[alerts].webhook_url` keeps working independently
  — both can coexist for "per-node detail to operator-Slack +
  fleet digest to ops-channel" patterns.

### Added — supporting

- `Action::SwitchContext(String)` was added in v1.11.0; v1.12 keeps
  it unchanged. The new modal and watchdog touch no semver-stable
  surface — both are gated by new optional config sections that
  default to off.
- `BatchAction` enum + `BatchModal` state struct on `App`.
- `SupervisorWatchdog` struct + `format_duration_short` helper.
- `FleetAggregator` struct + `FleetAlertEntry` + pure
  `ingest_snapshot` / `drain_if_window_elapsed` / `format_message`.

### Internals

- `App::with_overrides` now returns both a `BeeSupervisor` and an
  optional `SupervisorWatchdog`; the loops at `App::run` (tick
  handler) call `tick_supervisor_watchdog()` after the existing
  `try_wait` status refresh.
- `tick_fleet_aggregate()` joins the existing tick sequence
  alongside `tick_alerts()` — both are no-ops when their
  respective webhook URLs are unset, so the cost is one
  `Option::is_none()` per tick.

### Notes

- Tests: 459 lib tests (was 444), +15 covering watchdog backoff
  curves + budget enforcement, aggregator transition rules
  (Pass↔Fail / steady-state suppression / Unknown ignoring /
  recovery), message formatting, and `BatchAction` parsing.
- Semver-stable surfaces untouched. All new functionality is
  gated by opt-in config sections.

## [1.11.0] - 2026-05-13

The "fleet view" minor release. Closes the v1.x design gap
between bee-tui's multi-node *config* support (which had been
in place since v0.2) and its multi-node *visibility* — every
release through v1.10 still surfaced a single active node at a
time. Operators running 3-10 nodes had to hop manually to confirm
nothing was on fire.

S15 Fleet now polls every configured `[[nodes]]` entry in
parallel every 10 seconds against a cheap 3-endpoint probe
(`/health` + `/status` + `/stamps`) and aggregates the results
into one row per node — operator scans the table in two seconds
to answer "is anything red?", then `Enter` switches context to
whichever node needs attention.

### Added

- **S15 Fleet screen** — new tabbed screen at index 14. One row
  per `[[nodes]]` entry showing aggregate status (pass / warn /
  fail / loading), connected peer count, worst-batch stamp TTL,
  and `/health` ping. The header line carries the fleet roll-up
  (`4 configured · 3 pass · 1 warn`). The cursored row gets a
  why-line continuation explaining the status reason when it's
  not green. `↑↓` / `j k` move the cursor, `Enter` switches
  context to that node (calls the existing `switch_context`,
  lands on S1 Health), `r` re-polls immediately.
- **`:fleet` verb** — jumps to S15 from the command bar.
- **`Alt+5` hotkey** — completes the second-row screen jumps
  (Alt+1..Alt+5 → S11..S15).
- **`crate::fleet` module** — `FleetSnapshot`, `FleetRow`,
  `FleetStatus`, and `spawn_poller(nodes, cancel, interval)`
  returning a `watch::Receiver<FleetSnapshot>` and the
  operator-resync mpsc handle. The poller fans out via
  `FuturesUnordered` with a 5-second per-probe timeout — a
  slow or unreachable node times out without blocking the
  others. Pure `aggregate(...)` fn drives the status ladder
  (zero peers → Fail; < 4 peers → Warn; warming up → Warn;
  stamp TTL ≤ 24h → Fail; stamp TTL ≤ 7d → Warn; otherwise
  Pass). Tested independently of HTTP.
- **`Action::SwitchContext(String)` variant** — emitted by the
  Fleet screen on `Enter`; `App::handle_actions` dispatches it
  through the same `switch_context` path as `:context` and the
  Ctrl+N picker, so daemon teardown + alert-state reset (v1.9.1)
  apply identically.

### Internals

- `App::fleet_rx` / `fleet_resync_tx` fields hold the poller's
  handles. The poller spawns once at `App::with_overrides` and
  survives every `switch_context` — fleet visibility is
  node-agnostic, so rebuilding it on profile change would just
  waste a probe cycle. The `Fleet` component takes the rx
  during `build_screens` so context-switches naturally pick up
  the live data without re-wiring.
- `SCREEN_NAMES` grew to 15 entries; numeric-hotkey constants
  updated; `screen_keymap(14)` advertises the new `↑↓` /
  `Enter` / `r` bindings; `verb_category("fleet")` returns
  `"navigate"` so it appears in the right help-overlay group.

### Notes

- Tests: 444 lib tests (was 424), +20 covering `aggregate`
  ladder transitions, snapshot count partitioning, view-shape
  formatting, and TTL bucketing.
- Resource cost for a 6-node fleet: 3 endpoints × 6 nodes /
  10 s = 1.8 reqs/s total. Reasonable on every Bee size.
- Semver-stable surfaces (`view_for` / `compute_*_view`,
  `watch::*` snapshot shapes, CLI flags, `[ui]` config,
  `--once` exit codes + JSON shape) untouched. The new
  `crate::fleet` types and `Action::SwitchContext` variant
  are additive.

## [1.10.0] - 2026-05-13

The "awareness + discoverability" minor release. Four small UI
additions that close gaps where bee-tui's functionality existed
but the operator had to know the typed verb to find it: the
configured `[[nodes]]` list, what's running in the background,
how to reach screens past `Tab`'s reach, and the full verb
catalogue.

### Added

- **Node picker overlay** (`Ctrl+N` / `:nodes`) — opens a centred
  list of every `[[nodes]]` entry from `config.toml`. The cursor
  lands on the active node; ↑/↓ (or `j`/`k`) move it, `↵`
  switches, `Esc` or `Ctrl+N` close without switching. The active
  node is marked `●`, the `default = true` entry `★`. Internally
  it's a thin wrapper around the existing `switch_context` flow
  introduced in v1.6 — same teardown of `pubsub_subs` /
  `watch_refs` / `alert_state`, same rebuild of the watch hub
  and screens.
- **Background-task awareness chips on the top bar** — three
  hidden-when-zero chips appended after `ping`: `subs N`
  (`pubsub_subs.len()` — active PSS / GSOC subscriptions), `watch
  N` (`watch_refs.len()` — active `:watch-ref` daemons), and
  `alerts ●` (a green dot when `[alerts].webhook_url` is set).
  Operators no longer have to remember which daemons they
  started; the chips appear when something is running and
  disappear when it stops.
- **Numeric screen hotkeys** — `1`-`9` jump to S1-S9 (Health
  through Tags), `0` jumps to S10 (Pins), `Alt+1`-`Alt+4` reach
  the second-row screens S11-S14 (Manifest, Watchlist,
  FeedTimeline, Pubsub). `Tab` / `Shift+Tab` still cycle as
  before — the digit row is additive, for direct access when you
  know where you want to be. (Replaces the historical "number
  keys deliberately unbound" decision in `keys.md` — five years
  of cockpit navigation taught us digit-row jumps don't conflict
  with in-screen selection in practice.)
- **Verb catalogue in the help overlay** — `?` now opens a
  two-page overlay. The **Keys** page mirrors the previous
  cheatsheet (global + screen-specific) plus the new numeric and
  `Ctrl+N` rows. The **Verbs** page lists every `:verb` from
  `KNOWN_COMMANDS` (~50 entries) grouped by category — navigate,
  inspect, stamps & economics, uploads, durability, pubsub,
  mining / addresses, diagnostics & config, cockpit. `Tab`
  switches between pages while help is open.

### Docs

- `reference/keys.md` — gains rows for numeric jumps and
  `Ctrl+N`; new sections describing the node picker and the
  paged help overlay; the "What's not bound" section drops its
  "number keys for screen jump would conflict" line.
- `commands/bar.md` — adds `:nodes` next to `:context`.
- `commands/context.md` — re-titled and grown a "picker overlay"
  section explaining the wrapper.

### Internals

- New `HelpPage` enum on `App` (`Keys` / `Verbs`) for the paged
  help overlay; routed via `Tab` while help is visible.
- New `App` fields `nodes_picker_visible: bool`,
  `nodes_picker_selected: usize`. Both rendered via a new
  `draw_nodes_picker` helper that auto-sizes to the configured
  node count (clamped to 6..20 rows / 48..80 cols).
- New pure helper `verb_category(name) -> &'static str` powers
  the help-overlay grouping; a test asserts every verb in
  `KNOWN_COMMANDS` maps to a real category (no silent
  `"other"` fall-through when adding new verbs).

### Notes

- Tests: 424 lib tests (was 422), +2 covering verb category
  exhaustiveness and the canonical groupings.
- Semver-stable surfaces (`view_for` / `compute_*_view`,
  `watch::*` snapshot shapes, CLI flags, `[ui]` config,
  `--once` exit codes + JSON shape) untouched. The new node
  picker overlay and help-page Tab handling are additive.

## [1.9.1] - 2026-05-13

The "context-switch correctness + documentation catch-up" patch.
Two bugs that only surface when an operator hops between nodes via
`:context` are now closed, and three address-display gaps on S3 /
S4 / S9 were filled in. No surface changes; semver-stable APIs
untouched.

The release also brings the mdBook user manual and the rendered
VHS demo GIFs back in sync with the v1.2-v1.9 cockpit surface —
operators landing on the docs site or README now see screens and
verbs that actually exist in the shipped binary.

### Fixed

- **`:context` switch leaked pubsub + watch daemons to the new node** —
  prior to v1.9.1, switching contexts kept both `pubsub_subs` and
  `watch_refs` populated; the background tasks they hold reference an
  `Arc<ApiClient>` bound to the *previous* node, so PSS/GSOC frames
  and `:durability-check` results from the old node continued to
  arrive at the freshly-built S15 / `:watch-ref` screens after the
  switch. `switch_context` now drains and cancels both maps, so a
  fresh context starts with no inherited subscribers or watchers.
- **`:context` switch carried stale `alert_state` across nodes** —
  per-gate transition memory used to log webhook alerts for
  Health/Connectivity/Capacity/Cheques/Reserve/StampsTTL gates
  survived the switch, which could fire spurious webhooks (or
  suppress legitimate ones) when the new node's gate readings
  differed from the old. `switch_context` now installs a fresh
  `AlertState` scoped to the new context.
- **Full chunk references / peer addresses now reachable on S3 / S4 / S9** —
  every screen with addresses must expose them in full somewhere
  (truncation in table columns is fine; the full hex has to be
  reachable without scrolling away). Three screens were missing the
  continuation-line treatment introduced for S8:
  - S3 SWAP — `CheckRow` / `SettlementRow` now carry `peer_full`;
    the per-row continuation line emits `peer 0x<full>`.
  - S4 Lottery — `:rchash` result no longer truncates to an
    8-char prefix; the result row emits a `hash 0x<full>`
    continuation line.
  - S9 Tags — `TagRow` now carries `address_full`; the per-row
    continuation line emits `ref 0x<full>` (suppressed for
    Pending tags whose address is empty).
  11 insta snapshots were regenerated to lock the new layout.

### Docs

- **mdBook catch-up across v1.2 – v1.9** — `config.md` gains a
  `[pubsub]` section (history file, rotation, filter); `bar.md`
  documents 17 verbs added across v1.2-v1.9 (`:manifest*`,
  `:inspect`, `:durability-check`, `:plan-batch`, `:check-version`,
  `:config-doctor`, `:price`, `:basefee`, utility verbs,
  `:pubsub-filter*`, `:pubsub-replay`, `:grantees-list`);
  `keys.md` documents S11 through S15 keymaps; intro / first-run /
  README screen counts updated from "nine" to "fourteen".
- **Three new mdBook pages** — S12 Manifests, S13 Watchlist, and
  the `--once` CI mode page. SUMMARY.md de-duplicated (the
  `s14-feed-timeline` / `s15-pubsub` pages were referenced under
  both Screens and Commands, which broke `mdbook build`).
- **VHS demo refresh** — `cold-start.tape` now walks all 14
  screens (was 9); 5 new tapes added (S12 Manifests, S13
  Watchlist via `:durability-check`, S14 Feed Timeline, S15
  Pubsub, `--once` CI mode). All 9 GIFs (~6 MiB total) re-rendered
  against a live Sepolia node. README adds "Data screens (v1.2+)"
  and "CI mode (v1.3+)" sections embedding the new GIFs.

### Notes

- Tests: 422 lib tests, unchanged (the fix is at the app-state
  layer; the snapshot suite covers the address-display regression).
- Semver-stable surfaces (`view_for` / `compute_*_view`,
  `watch::*` snapshot shapes, CLI flags, `[ui]` config, `--once`
  exit codes + JSON shape) untouched.

## [1.9.0] - 2026-05-08

The "pubsub durability" release. Two small follow-ups close the
overnight-subscription loop opened by v1.8's `[pubsub].history_file`
writer: rotation keeps disk usage bounded, and a replay verb loads
prior sessions back into S15 for visual analysis.

### Added

- **`[pubsub].rotate_size_mb` + `[pubsub].keep_files`** — when the
  history file crosses the size threshold (default 64 MiB), bee-tui
  rolls it over to `<path>.1` (and shifts older rotations to `.2` ..
  `.N`, dropping the oldest beyond `keep_files`, default 5). Set
  `rotate_size_mb = 0` to disable rotation. The rotation is
  serialised through the same mutex that orders concurrent appends,
  so PSS + GSOC watchers can't race a rename.
- **`:pubsub-replay <path>`** — load a prior session's pubsub-history
  JSONL back onto the S15 timeline (oldest → newest, capped at the
  500-message ring). Bad lines are skipped with a warn log; replay
  does not start any watchers.

### Notes

- Tests: 422 lib tests (was 411 in v1.8.0), +10 covering rotation,
  replay round-trip, MAX_MESSAGES cap, bad-line skip, and hex
  decoder edges.

## [1.8.0] - 2026-05-08

The "polish + read-side ACT" release. Three small features
extend the v1.7 surfaces — pubsub gets a filter and an optional
JSONL history-file writer; ACT grantee lists become readable from
the cockpit (cockpit + `--once`).

### Added — uploads / ACT

- **`:grantees-list <ref>`** — read-only `GET /grantee/{ref}`
  fetch. Cockpit shows `count` + first 3 grantee public keys
  (with truncation suffix); `--once grantees-list` emits the full
  array as JSON for CI pipelines that want to assert a known
  builder is still on the list before treating an upload as
  published. The read-side foundation for a future S16 ACT
  Grantees screen with create / patch.

### Added — pubsub polish

- **`[pubsub].history_file`** — opt-in JSONL writer that
  appends every delivered PSS / GSOC frame to a file as it
  arrives. Off by default (no surprise disk writes); when set,
  the file is opened with mode `0600` so payloads aren't
  world-readable on multi-user hosts. Each line is one JSON
  object with `received_unix`, `kind`, `channel`, `size`, and
  `payload_hex`. Useful for offline analysis of overnight
  subscriptions.
- **`:pubsub-filter <substring>`** + **`:pubsub-filter-clear`** —
  case-insensitive substring filter on the S15 timeline. Matches
  against channel hex or smart-preview content; the underlying
  ring still receives every message (filtering is
  presentation-only) so clearing the filter restores the full
  history.

### Internals

- New `[pubsub]` config section + `crate::pubsub::HistoryWriter` /
  `open_history_writer` / `append_history` plumbing. Watchers
  serialise their appends through a shared `tokio::sync::Mutex`
  so JSONL stays well-formed under concurrent delivery.
- New `Pubsub::matches_filter` (pure, 4 unit tests) + the screen
  pre-collects filtered rows before render so the cursor lines
  up with what's actually visible.
- 411 lib tests + integration suite passing.

## [1.7.0] - 2026-05-08

The "pubsub + cross-check" release. Two features land: pubsub
finally has a surface in bee-tui (the receiver-side complement to
v1.3's writer verbs), and `:durability-check` / `:watch-ref` gain
an optional independent cross-check against a swarmscan-style
indexer.

Two commits since v1.6.0 (`ddc36a7`, `214c547`).

### Added — pubsub

- **S15 Pubsub watch screen** — merged timeline of PSS topic
  subscriptions and GSOC `(owner, identifier)` subscriptions.
  Each delivered frame becomes one row with kind glyph, channel
  short hex, payload size, and smart-preview (ASCII when ≥75 %
  printable, hex otherwise). Cursor + PgUp/PgDn navigation;
  `c` clears the timeline; ring-buffered to 500 messages.
- **`:pubsub-pss <topic>`** / **`:pubsub-gsoc <owner> <id>`** —
  open a Bee WebSocket against `/pss/subscribe` or
  `/gsoc/subscribe` and forward every delivered frame into S15.
  Topic accepts the same forms as `:feed-probe` (64-hex literal
  or string-form via `Topic::from_string`).
- **`:pubsub-stop [sub-id]`** — cancel one or all active
  subscriptions. Re-issuing for an already-watched
  `(topic)` / `(owner, id)` errors with a clear message — no
  silent duplicate sockets.
- New `crate::pubsub` module owns spawn_pss_watcher /
  spawn_gsoc_watcher (async setup → tokio task with select! over
  recv + cancel) plus pure ASCII / hex / smart preview formatters
  (9 unit tests). The screen has 5 more covering ring eviction,
  cursor clamping, clear-key behavior.

### Added — durability cross-check

- **Swarmscan cross-check** (`[durability].swarmscan_check`).
  Off by default. When on, every completed `:durability-check`
  (single-shot or via the `:watch-ref` daemon) hits an indexer
  URL (templated, defaults to swarmscan's public chunk endpoint)
  for an independent "does the network see this ref?" answer.
  New `DurabilityResult.swarmscan_seen` field (`Some(true)` /
  `Some(false)` / `None`); surfaces in summary line, S13 row
  detail, and `--once durability-check` JSON.
- Useful for catching cases where the local Bee returns a chunk
  from cache that no peer in the network actually still serves.
  Closes the second half of Batch E's deferred trail from the
  v1.3 plan.

### Internals

- New `App::pubsub_subs: HashMap<String, CancellationToken>` for
  the active subscription registry; matches the
  `App::watch_refs` shape introduced in v1.6.
- New `pubsub_msg_tx/rx` mpsc channel feeds delivered frames into
  the S15 screen via the Tick handler.
- New `App::durability_check_options()` reads
  `[durability].swarmscan_check` per-walk, so config edits via
  future `:context` re-loads take effect on the next check.
- 407 lib tests + integration suite passing.

## [1.6.0] - 2026-05-08

The "watch + walk" release. Two features extend v1.5's primitives
into long-running and historical surfaces: the v1.5 single-shot
`:feed-probe` graduates into a full S14 Feed Timeline screen with
cursor navigation; v1.5's BMT-verified `:durability-check` gets a
daemon mode that re-runs it periodically and feeds the rolling
history into the existing S13 Watchlist.

Two commits since v1.5.0 (`74a0576`, `b6c77e0`).

### Added — feeds

- **S14 Feed Timeline screen** — walks a feed's history backward
  from the latest index and renders each entry's index, age,
  payload size, and (when reference-shaped) embedded Swarm
  reference. Misses (404 / parse error) render dim so gaps are
  visible at a glance. Cursor navigation (`↑↓` / `jk` / PgUp /
  PgDn). Default 50 entries, hard-capped at 1000.
- **`:feed-timeline <owner> <topic> [N]`** — cockpit verb that
  triggers the walk and switches to S14 with a spinner while the
  background fetch runs. Topic input accepts the same forms as
  `:feed-probe` (64-hex literal or arbitrary string).
- **`--once feed-timeline`** — JSON output with full entries
  array. CI gates can assert `entries[0].index` strictly advances
  across runs or that `error` count stays under a threshold.
- New `crate::feed_timeline` module with bounded-parallel walker
  (8-way `futures::join_all`) and pure SOC-bytes parser. 3 unit
  tests cover the parser + summary; the screen ships 6 more
  covering loading-state transitions and cursor clamping.

### Added — durability

- **`:watch-ref <ref> [interval]`** — daemon mode for
  `:durability-check`. Spawns a tokio task that re-runs the full
  BMT-verified walk every `interval-secs` (default 60, clamped
  10..=86400) and feeds each result into the existing S13
  Watchlist ring. Re-issuing for a watched ref cancels the prior
  daemon — convenient for changing the interval without an
  explicit stop.
- **`:watch-ref-stop [ref]`** — cancels active daemons. With no
  arg, cancels every active one; with a `<ref>` arg, cancels just
  the matching daemon.
- Pairs cleanly with v1.4's `[alerts].webhook_url` — durability
  gate transitions on the watched ref now ping the operator's
  webhook through the existing alerter path.

### `--once` CI mode

One new verb (`feed-timeline`) — total verbs across cockpit +
`--once` is now 42 cockpit / 23 `--once`.

### Internals

- New `App::watch_refs: HashMap<String, CancellationToken>` —
  daemons keyed by reference hex. Each token is a child of
  `root_cancel` so quit unwinds them like every other spawned
  task.
- New `feed_timeline_tx/rx` mpsc channel mirrors the
  `durability_tx` plumbing for funneling background results into
  S14.
- 392 lib tests + integration suite passing.

## [1.5.0] - 2026-05-08

The "publish + verify" release. Three features bundle into the
"v1.4 unblocked these" theme: now that v1.4 shipped `:upload-file`
and v1.2 shipped `:durability-check`, this release closes their
natural gaps — recursive directory uploads, end-to-end BMT
verification of every fetched chunk, and the receiver-side feed
lookup that pairs with v1.3's `:gsoc-mine` / `:pss-target` writer
verbs.

Three commits since v1.4.0 (`e30b3f6`, `8c0f2a6`, `19711ac`).

### Added — uploads

- **`:upload-collection <dir> <batch>`** — recursive directory
  upload via tar `POST /bzz`. Hidden entries (`.git`, `.env`, …)
  and symlinks are skipped; an `index.html` at the collection
  root auto-becomes the manifest's default index. Caps mirror
  `:upload-file`: 256 MiB total, 10k entries. Deterministic
  sort order, so identical inputs produce identical Swarm
  references. Available as `--once upload-collection` for CI
  snapshot-publish workflows; emits JSON with `reference`,
  `entry_count`, `total_bytes`, `default_index`.
- New `crate::uploads` module with the directory walker (7 unit
  tests against tempfile fixtures pin the traversal rules).

### Added — durability

- **BMT verification** on every chunk fetched by
  `:durability-check`. New `chunks_corrupt` bucket separate from
  `chunks_lost` (404s) and `chunks_errors` (other failures), so
  operators can tell bit-rot / hostile-peer behaviour from
  network drops. Default on (one keccak per chunk; cost is
  negligible vs the network round-trip). New
  `durability::CheckOptions { bmt_verify }` exposes a way to
  disable for very large walks; surfaced in `--once
  durability-check` JSON as `chunks_corrupt` + `bmt_verified`,
  and in S13 Watchlist row detail as `N corrupt · BMT`.

### Added — feeds

- **`:feed-probe <owner> <topic>`** — read-only lookup of the
  latest update of a Swarm feed. Surfaces index, timestamp,
  payload size, and (when the payload is reference-shaped) the
  embedded Swarm reference. Topic input accepts 64-hex literal
  or any other string (keccak256-hashed via
  `Topic::from_string`, matching bee-js convention). The
  receiver-side complement to v1.3's `:gsoc-mine` and
  `:pss-target`. Available as `--once feed-probe` for CI gates
  that assert a known feed advances.
- New `crate::feed_probe` module with pure parse_args /
  parse_update functions (7 unit tests).

### `--once` CI mode

Two new verbs (`upload-collection`, `feed-probe`) — total verbs
across cockpit + `--once` is now 39 cockpit / 22 `--once`.

### Internals

- `bee::swarm::bmt::calculate_chunk_address` round-trip now
  pinned in a unit test against a flipped-byte tampered chunk;
  the durability walk's correctness depends on this contract.
- 383 lib tests + integration suite passing.

## [1.4.0] - 2026-05-08

The "operator-context" release. Six features bundle into a coherent
"the cockpit doesn't just show you Bee — it tells you when something
breaks, what things cost, and lets you publish a file without
shelling out" theme.

Six commits since v1.3.0 (`d024c4d`, `80cfe56`, `0c216a5`, `fbcbbc0`,
`042fe30`, `4456f96`).

### Added — observability

- **Webhook health-gate alerts** (`[alerts].webhook_url`). Every
  health-gate transition (Pass↔Warn, Pass↔Fail, etc.) becomes one
  Slack/Discord-compatible POST. Per-gate debounce (default 5 min)
  prevents flapping; Unknown transitions are suppressed so cockpit
  startup never spams. Off by default — fresh installs make no
  outbound traffic.
- **Stamp TTL gate** in `Health::gates_for_with_stamps` — aggregates
  over usable batches and reports the worst-case TTL: Pass when all
  batches have >7d, Warn under the planning threshold, Fail under
  the 24h urgent threshold. Plumbed through the alerter, so silent
  batch expiries trigger webhooks the same way reachability outages
  do, and surfaced in `:diagnose` bundles.

### Added — cost context

- **`:price`** — fetches xBZZ → USD spot price from Swarm's public
  token service. No configuration required.
- **`:basefee`** — fetches Gnosis-chain basefee + tip via JSON-RPC.
  Requires `[economics].gnosis_rpc_url` (typically the same URL as
  Bee's `--blockchain-rpc-endpoint`). Surfaces a clear "configure"
  hint when unset.
- **Live Market tile on S3 SWAP** (`[economics].enable_market_tile
  = true`). Always-on tile rendering `BZZ ≈ $X.XXXX` and `gas: B
  base + T tip = N gwei`, refreshed every 60 s. Off by default;
  fresh installs still make no outbound traffic without an explicit
  opt-in.

### Added — version drift + config drift

- **`:check-version`** — compares the running Bee version against
  the latest GitHub release; lenient SemVer parser handles `v`
  prefixes, RC suffixes, and `dirty` builds.
- **`:config-doctor`** — read-only audit of `bee.yaml` for
  deprecated keys + recommended values. Ports the rule set from
  swarm-desktop's `migration.ts`. Never modifies the file.

### Added — uploads

- **`:upload-file <path> <batch>`** — uploads a single local file
  via `POST /bzz` and returns the Swarm reference. 256-MiB ceiling
  protects the cockpit's event loop. Content type guessed from
  ~15 common extensions; unknown falls back to
  `application/octet-stream`. Available as `--once upload-file` for
  CI snapshot-publish workflows; emits structured JSON with the
  reference + size + batch_id.

### `--once` CI mode

`--once` now supports five new verbs (`check-version`,
`config-doctor`, `price`, `basefee`, `upload-file`) — total verbs
across cockpit + `--once` is now 37.

### Internals

- New `crate::alerts` module with `Alert`, `AlertState`, and a
  Slack/Discord-compatible `fire(...)` async helper. Tested via
  injectable `SystemTime` for deterministic debounce assertions.
- New `crate::economics_oracle::spawn_poller` — long-running tokio
  task that emits `EconomicsSnapshot` updates over a watch channel.
- `Health::gates_for_with_stamps` (additive over `gates_for`) so
  the existing visual S1 Health screen keeps the same gate count
  it had before; only the alerter and `:diagnose` bundle pull the
  stamp gate.
- 365 lib tests + integration suite passing.

## [1.3.0] - 2026-05-08

The "CI cockpit" release. `--once` CI mode + `:plan-batch` ship the
sleeper bet from the research synthesis: every preview verb is now
usable from a shell pipeline or GitHub Action without parsing TUI
output, and operators get unified topup-or-dilute-or-both decisions
in one verb.

Three commits since v1.2.0 (`142ce87`, `2713c79`, `912f9ac`):

### Added — `--once` CI mode

- **`bee-tui --once <verb> [args…]`** — runs a single verb without
  launching the TUI, prints to stdout, exits with `0` (ok), `1`
  (unhealthy / network failure), or `2` (usage error). The whole
  TUI runtime is bypassed; pure-local verbs do nothing
  network-bound, Bee-API verbs do a one-shot fetch and exit.

- **15 verbs** in the first cut, split between pure-local and
  Bee-API:

  *Pure-local* (no Bee call):
  `hash <path>` · `cid <ref> [m|f]` · `depth-table` ·
  `pss-target <overlay>` · `gsoc-mine <overlay> <id>`

  *Bee-API* (one-shot fetch, no watch hub):
  `readiness` (gateway-proxy-style: `health.status=='ok' &&
  depth in [1, 30]`) · `version-check` ·
  `inspect <ref>` · `durability-check <ref>` · `buy-preview` ·
  `buy-suggest` · `topup-preview` · `dilute-preview` ·
  `extend-preview` · `plan-batch`

- **`--json`** flag emits a single JSON object on stdout instead of
  the human-readable line: `{ verb, status, message, data }`. The
  `data` field carries each verb's structured fields so CI can
  parse without regex on the human line.

- **No tracing init in --once mode.** Stdout is clean — shell
  pipelines `grep` without filtering log noise.

### Added — `:plan-batch` unified topup+dilute preview

- **`:plan-batch <prefix> [usage-thr] [ttl-thr] [extra-depth]`** —
  read-only run of beekeeper-stamper's `Set` algorithm. Tells the
  operator whether the batch needs topup, dilute, both, or nothing,
  plus the BZZ cost. Closes the gap from the three single-leg
  preview verbs (`:topup-preview`, `:dilute-preview`,
  `:extend-preview`) which each answered one piece of a multi-step
  decision in isolation.

- **Defaults match the cross-ecosystem convention** (gateway-proxy /
  swarm-gateway / beekeeper-stamper): usage threshold 0.85, TTL
  threshold 24h, dilute by +2 depth (4× capacity).

- **Four action variants:**
  - `None` — batch healthy against both thresholds.
  - `Topup` — TTL below threshold, usage healthy.
  - `Dilute` — usage above threshold, post-dilute TTL still healthy.
  - `TopupThenDilute` — both fail; topup pre-dilute to a TTL high
    enough that post-dilute (÷ `2^extra_depth`) clears the
    threshold.

- **Immutable batch handling.** Immutable batches can't dilute; the
  algorithm flags this and either plans topup-only (when TTL needs
  it) or returns `None` with a reason explaining why we can't act
  on the high usage.

- **Also via `--once plan-batch`.** Exits `1` when an action is
  recommended (so a CI job can gate on "this batch needs human
  attention"), `0` only when no action is needed.

### Notes

- **Test count**: 350+ lib + ~80 insta integration tests; clippy +
  fmt clean.
- **bee-rs dependency**: still 1.6 (no bump required).
- **Semver**: every addition is additive — no breaking changes
  to the v1.0-committed surface.
- **`--once` exit codes** are part of the v1.3-committed CLI
  contract; we'll preserve `0` / `1` / `2` semantics in future
  minor bumps and grow the `data` field with new keys rather than
  rename existing ones.

## [1.2.0] - 2026-05-08

The "audit cockpit" release. Two new screens (S12 Manifests, S13
Watchlist), seven new verbs, and a Cockpit log tab — all read-only,
all PLAN-clean. bee-tui evolves from "inspects the node" to
"inspects the node *and* the data."

Five batches shipped over a single development cycle (commits
`05b44b4` … `0a0e975`):

- **Batch A** — five pure-local utility verbs that never hit the
  Bee API.
- **Batch B** — Cockpit log tab + per-peer reserve-state in the S6
  drill (with outlier coloring against `bee-scripts/bad-status.sh`).
- **Batch C** — S12 Manifests screen + `:manifest` / `:inspect`
  verbs (the v1.2 flagship).
- **Batch D** — `:diagnose --pprof[=N]` to bundle Bee's CPU profile
  + execution trace.
- **Batch E** — `:durability-check` + S13 Watchlist screen.

### Added — utility verbs (Batch A)

- **`:hash <path>`** — Swarm reference of a local file or directory,
  computed offline. Mirrors `swarm-cli hash`. Single files stream
  through bee-rs's `FileChunker`; directories use `hash_directory`.
- **`:cid <ref> [manifest|feed]`** — encode a 32-byte reference as a
  multibase CIDv1 string. Defaults to `manifest`; encrypted (64-byte)
  refs are rejected with a clear error.
- **`:depth-table`** — print the canonical depth → effective-bytes
  table to a temp file (the cockpit's command bar is too narrow for
  18 rows).
- **`:gsoc-mine <overlay> <id>`** — pure CPU work that finds a
  `PrivateKey` whose SOC at `(identifier, owner)` lands in the
  target neighborhood. Matches bee-js `gsocMine`.
- **`:pss-target <overlay>`** — derive Bee's max-target prefix
  (the first 4 hex chars). Mirrors bee-js
  `Utils.makeMaxTarget`.

### Added — UX wins (Batch B)

- **Cockpit log tab.** A 7th tab in the bottom log pane that surfaces
  cockpit-internal tracing events (everything bee-tui emits that
  isn't `bee::http`). Today these go to `bee-tui.log` on disk;
  operators had to leave the cockpit to read them. Bounded at 500
  entries; rendered as `TS LEVEL TARGET  message` with the
  `bee_tui::` prefix trimmed.
- **Per-peer reserve-state in S6 drill.** Drill fan-out grows from
  four to six parallel calls so each peer row surfaces
  `storage_radius / reserve_size / pullsync_rate / batch_commitment`
  alongside the existing balance / cheques / settlement / ping. The
  `batch_commitment` cell paints red when |peer − local| / local
  > 5%, mirroring the outlier filter in `bee-scripts/bad-status.sh`
  so operators reading both tools see the same warnings.

### Added — S12 Manifests + universal `:inspect` (Batch C)

- **S12 Manifests screen — Mantaray tree browser.** New 11th screen
  for browsing any reference as a Mantaray manifest tree. Tree
  rendering is flat: each row is `(depth, glyph, label, content-type,
  target-ref, state-hint)`. `↑↓` navigate, `Enter` toggles a fork's
  expand state — kicks an async fetch when the child isn't loaded.
  Per-fork load state machine (`Idle / Loading / Loaded / Error`)
  surfaces "loading…" + "error: …" inline so the operator sees what's
  in flight.
- **`:manifest <ref>`** — fetch the chunk + open S12 with the tree
  rooted on `<ref>`.
- **`:inspect <ref>`** — universal "what is this thing?" verb. Fetches
  one chunk and tries `MantarayNode::unmarshal`. On manifest, jumps
  to S12; on raw chunk, prints "raw chunk · X bytes · not a manifest"
  to the command-status row.
- **`Component::as_any_mut`** trait extension. Optional downcast hook
  so verbs like `:manifest` can reach the concrete `Manifest` screen
  type and call its load() method without rebuilding the screens
  vector. Default returns `None`; opt-in per screen.
- **bee-rs gap closed caller-side.** Bee-rs 1.6 has `MantarayNode` +
  `unmarshal` but no recursive-load API. The new `manifest_walker`
  module implements lazy fork-loading on top of the chunk-download
  primitive. When 1.7 ships `load_recursively` we can swap to it
  transparently.

### Added — pprof bundle (Batch D)

- **`:diagnose --pprof[=N]`** — extends the existing `:diagnose` verb
  with an optional pprof bundle. Spawns parallel fetches of
  `/debug/pprof/profile?seconds=N` (CPU profile) and
  `/debug/pprof/trace?seconds=N` (execution trace) and writes each
  alongside the snapshot text in a fresh `bee-tui-diagnostic-<ts>/`
  directory. Default sampling window is 60s; explicit values clamp
  to `[1, 600]`.
- 404 fallback. When Bee's debug API isn't enabled, the helper
  surfaces a clear "add `--debug-api-enable=true` to your Bee
  start args" hint instead of a cryptic HTTP error.
- No tar dependency. The bundle is a self-contained directory; the
  operator runs `tar -czf` themselves to ship a support bundle.

### Added — durability check + S13 Watchlist (Batch E)

- **`:durability-check <ref>`** — walks the chunk graph rooted at
  `<ref>` and records the result. Mantaray refs walk recursively
  (root + every fork's `self_address`); raw refs are a single fetch.
  Distinguishes `chunks_lost` (a 404 = data truly gone) from
  `chunks_errors` (any other failure = retry). Bounded by
  `MAX_CHUNKS_PER_WALK = 10000` so very large manifests give a
  partial answer rather than pinning the cockpit.
- **S13 Watchlist screen.** New 12th screen. History of every
  `:durability-check` invocation as a row with status (OK /
  UNHEALTHY) · kind (manifest | chunk) · ref · detail-line · age.
  Bounded ring of 50 entries; newest first. Header shows healthy
  vs unhealthy counts.
- **`:watchlist`** — jump straight to S13.

### Notes

- **Test count**: 343 lib + ~80 insta integration tests; clippy +
  fmt clean.
- **bee-rs dependency**: still 1.6 (no bump required).
- **Semver**: every addition is additive — no breaking changes
  to the v1.0-committed surface (`view_for` / `compute_*_view` pure
  fns, `bee_tui::watch::*` snapshot shapes, CLI flags, `[ui]`
  config schema).

## [1.1.0] - 2026-05-08

Large feature release. Three operator-tiers of cockpit polish on top
of the supervisor / log-pane / refresh-preset work that landed earlier
in the cycle:

- **Tier 1**: predictive stamp economics, pending-tx age column with
  threshold colouring, bee-supervisor log rotation.
- **Tier 2**: stamp dry-run preview verbs (topup / dilute / extend /
  buy / buy-suggest), opt-in Prometheus `/metrics` endpoint,
  `:probe-upload` single-chunk end-to-end probe.
- **Tier 3**: S11 Pins screen with on-demand integrity checks, S6
  saturation rollup line in the header.

Plus three operator-feedback fixes: full-ID copy on every cursor-driven
screen, horizontal scroll in the log pane, autocompleting `:command`
suggestion popup.

### Added — Tier 3 (extra cockpit screens & summaries)

- **S11 Pins screen.** New tenth-screen tab listing every pinned root
  with on-demand integrity checks. `Enter` checks the cursored pin;
  `c` walks every pin in sequence; `s` cycles sort modes
  (Reference / BadFirst / TotalChunks). Async fetch results drain via
  an mpsc channel so the UI stays responsive while a long check runs.
  Status ladder: `Idle / Checking / Ok / Failed`; failed pins float to
  the top in BadFirst mode.
- **S6 saturation rollup line.** Header shows `✗ STARVING X of N` or
  `✓ all N relevant bins healthy` so the operator reads the network
  health gate at a glance without scanning every bin row. Drill-down
  now reveals the full peer overlay; a `selected: ...` detail line
  above the footer makes the cursored ID copyable on every
  cursor-driven screen.

### Added — Tier 2 (cost-preview verbs, Prometheus, probe-upload)

- **Four stamp dry-run preview verbs.** `:topup-preview`,
  `:dilute-preview`, `:extend-preview`, `:buy-preview` predict
  capacity / cost / TTL before any chain-bearing write. Pure
  `BigInt` arithmetic against the live stamp snapshot — no Bee call
  required. Short batch-ID prefix matching, binary / decimal / shorthand
  size parsing (`64MiB`, `1.5GB`, `4096`), human-duration parsing
  (`30d`, `2w`, `5h30m`), PLUR / xBZZ amount parsing.
- **`:buy-suggest <size> <duration>`** — inverse of `:buy-preview`.
  Given target volume + duration, returns the minimum
  `(depth, amount)` tuple that satisfies it, plus the projected cost.
  `chunks_needed = ceil(target_bytes / 4096)`,
  `depth = max(17, ceil(log2(chunks_needed)))`,
  `amount = ceil(target_secs / 5) × current_price`.
- **Opt-in Prometheus `/metrics` endpoint.** Hand-rolled tokio
  `TcpListener`-based HTTP/1.1 server emitting ~30 metrics across
  status / chain / stamps / tx / swap / lottery / topology / network /
  self-request namespaces. Per-batch labels on stamp gauges; a
  synthesised `bee_tui_status_depth_radius_gap` gauge derived from
  `(committed_depth - storage_radius)`. Off by default; enable via
  `[metrics] enabled = true, addr = "127.0.0.1:9101"`. 5 s
  per-connection timeout; cancellation-token shutdown so quit unwinds
  the listener cleanly.
- **`:probe-upload`.** Single-chunk end-to-end probe. Synthesises a
  unique 4104-byte chunk (8-byte span + 16-byte timestamp + zero-pad),
  uploads it under the operator-supplied stamp, retrieves it back,
  prints a four-line timing breakdown (upload / propagate / retrieve /
  total). Each invocation produces a fresh reference so a probe is
  never served from local cache.

### Added — Tier 1 (predictive economics, pending-tx age, log rotation)

- **S2 predictive stamp economics.** Drill pane now shows theoretical
  capacity in human bytes, total cost-in-xBZZ, and days-until-expiry
  derived from `amount × 2^depth / 1e16`. Stamp status now uses three
  TTL bands (`TOPUP_SOON_SECS = 7d`, `TOPUP_URGENT_SECS = 24h`) on top
  of utilization, so a low-utilization batch about to expire is no
  longer reported as `Healthy`.
- **S8 pending-tx age column.** Each pending tx shows time-since-creation
  with `PENDING_TX_WARN_AGE_SECS = 300` / `PENDING_TX_FAIL_AGE_SECS =
  1800` colouring (5 min warn, 30 min fail). A continuation line under
  each pending row exposes the full `to:` address + transaction hash
  in plain text so terminal-native click-drag copies them straight to
  a block explorer.
- **bee-supervisor log rotation.** Spawned-Bee stdout/stderr now route
  through a size + `keep_files` rotating writer (defaults: 64 MiB,
  5 files) instead of `Stdio::from(File)`. Atomic rename moves
  `base → .1 → .2 → ...` to keep logfmt entries intact across rotation
  boundaries. The bee-log tailer detects rotation by inode mismatch +
  size shrink and reopens the file cleanly. Configure with
  `[bee.logs] rotate_size_mb = 64, keep_files = 5`.

### Added — operator-feedback fixes

- **Selectable full IDs on every cursor-driven screen.** A `selected:`
  detail line lives above the footer on S2 (Stamps), S6 (Peers), and
  S11 (Pins). Whatever row the cursor sits on has its full identifier
  (batch ID / overlay / reference) rendered in plain text on that
  line — terminal-native click-drag copies it without expanding the
  drill or chasing through truncated columns.
- **Horizontal scroll in the log pane.** `Shift+←` / `Shift+→` step
  8 columns left / right through the active tab. `Shift+End` resumes
  tail mode and resets both axes. Switching tabs (`[` / `]`) also
  resets. Title strip shows `→ N` when h-scrolled so the operator can
  see which tab+offset they're on. Long Bee log lines and wide
  bee::http tab tables no longer truncate.
- **Autocompleting `:command` suggestion popup.** A vertical popup
  above the command bar lists matching commands from a 22-entry
  catalog with one-line descriptions. As you type, the list filters
  by prefix. `Up` / `Down` navigate; `Tab` accepts the highlighted
  suggestion. Auto-scrolls when the filtered list exceeds 10 visible
  rows. Discoverability for every verb without leaving the keyboard.

### Changed

- **S7 Network shows full overlay + ethereum addresses.** Previously
  truncated to first-4-last-4 hex; operators couldn't click-drag to
  copy them for block-explorer / support-thread use. Now rendered
  in full (overlay = 64 chars, ethereum = 0x + 40) on their own
  identity lines. Mouse-mode is still off so terminal-native
  selection just works.
- **S2 Stamps drill header shows the full batch ID.** Same
  rationale; the drill-pane is now the place to copy a batch ID.

### Added — earlier in the cycle (post-1.0.0, pre-Tier-1)

- **bee-tui-only User-Agent + Bee HTTP tab filtering.** Every Bee
  API call now ships `User-Agent: bee-tui/<version>` (set on the
  reqwest client we hand into `bee::Client::with_http_client`).
  The Bee HTTP tab parses each `node/api*` log line for the
  `user_agent` / `user-agent` / `useragent` / `ua` field
  (case-insensitive) and drops lines whose value contains
  `bee-tui` — leaving only third-party clients (curl, swarm-cli,
  browser) on that tab. The cockpit's bee::http tab — fed from
  bee-tui's own client tracing — still shows everything bee-tui
  itself called. The two tabs are now genuinely disjoint when
  Bee logs the User-Agent header.

  If Bee doesn't log User-Agent (older builds), the filter is a
  silent no-op and bee-tui's calls still appear on Bee HTTP. No
  harm done; bee::http remains the trust anchor.

- **`[ui].refresh` polling-cadence preset.** Three presets:
  - `live` — original 2 s health / 5 s topology+tags / 30 s
    swap+lottery+transactions / 60 s network. Use when actively
    diagnosing.
  - `default` (NEW DEFAULT) — calmer 4 s health / 10 s topology+tags
    / same mid + slow tiers as live. About half the request volume.
  - `slow` — minimal 8 s / 20 s / 60 s / 120 s. For
    "leave it open all day" monitoring.

  Operators upgrading from a prior install will see the new default
  cadence unless they explicitly set `refresh = "live"` to keep
  the old feel. UX impact of the slowdown: ping indicator updates
  every 4 s instead of 2 s; bin saturation / topology refresh every
  10 s instead of 5 s. Drills, the rchash benchmark, and every
  Enter-fetch are unaffected (they're synchronous, not poll-driven).

- **Scrollable log pane.** `Shift+↑` / `Shift+↓` step one line back
  / forward through the active tab's history. `Shift+PgUp` /
  `Shift+PgDn` step ten. `Shift+End` resumes auto-tail. While
  scrolled back, the title strip shows a `paused N ↑` indicator in
  warn-yellow so it's impossible to forget you're not seeing the
  latest. New entries arriving on the active tab auto-bump the
  offset to keep the visible window anchored on the same content
  rather than drifting upward as new lines push the old ones up.
  Switching tabs (`[` / `]`) resets to tail mode.

- **Bee HTTP tab fed from server-side log (increment 4 of 4).**
  Lines with `logger=node/api*` (covers `node/api`,
  `node/api/access`, etc. across Bee versions) now route to the
  Bee HTTP tab instead of the severity tabs. The Bee-HTTP check
  wins over severity routing — an `error`-level line from
  `node/api` lands on Bee HTTP, not Errors. Reason: operators
  looking at Errors want to see *infrastructure* problems, not
  4xx replies to misbehaving clients.

  Caveat documented inline: bee-tui's *own* requests against Bee
  also produce these lines from Bee's perspective. There's no
  reliable way to filter them server-side. The cockpit's
  `bee::http` tab — fed from bee-tui's own client tracing — is
  the right place to see "what bee-tui called"; the Bee HTTP tab
  is "everything Bee served", which usually overlaps but doesn't
  have to (e.g. when you also have curl / swarm-cli hitting the
  node from a separate shell).

  This is the final increment of the four-part log-pane redesign.
  bee-tui is now a fully-shaped log cockpit when [bee] is set:
  spawn → tail → parse → route → render. With [bee] unset, the
  legacy "connect to running Bee" flow keeps the bee::http tab
  populated and the four severity tabs explain themselves.

- **Bee log file-tail + parser + severity routing (increment 3 of 4).**
  When bee-tui spawns Bee (`[bee]` configured), a background tailer
  follows the supervisor's captured log file at 200 ms cadence.
  Each new line is parsed via the new `bee_log::parse_line` (a
  hand-rolled scanner for Bee's quoted-key logfmt format —
  `"time"="..." "level"="debug" "logger"="node/..." "msg"="..." extras...`)
  and routed by `level` to the matching tab (`error`/`err`/`fatal` →
  Errors, `warning`/`warn` → Warning, `info` → Info, `debug`/`trace`
  → Debug). Unrecognised levels are dropped so a future Bee build
  with a new severity doesn't get silently misfiled. Parser is pure
  + extensively tested against verbatim live-log samples (pseudosettle
  payment lines, batchservice block-height updates, libp2p stream-reset
  errors). The tailer respects `root_cancel` so quit unwinds it.
- **Tabbed bottom log pane (increment 2 of 4).** Replaces the
  single `bee::http` strip with a six-tab pane: Errors / Warn /
  Info / Debug (filled by Bee's log in increment 3), Bee HTTP
  (Bee's served-request log in increment 4), and bee::http (the
  legacy bee-tui own-request tail, kept as the trust anchor).
  Tab strip in the pane title; counts on each Bee-side tab.
  - `[` / `]` cycle tabs (lazygit / k9s pattern; no conflict
    with `Tab` / `Shift+Tab` which switch screens).
  - `+` / `-` grow / shrink the pane height by one line each
    (clamped to 4..24, default 10).
  - The active tab + last height are persisted across launches
    in `~/.local/state/bee-tui/state.toml` (XDG state dir; falls
    back to `data_local_dir` on macOS / Windows). Override the
    location with `$BEE_TUI_STATE`.
  - The four severity tabs and Bee HTTP tab show a placeholder
    until increment 3 wires the supervisor's log tail through.
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
