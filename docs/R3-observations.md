# R3 — long-run live observation

A structured pre-1.0 audit. The intent of R3 was to run bee-tui
against a live Bee node for a long-form session and note every
papercut. Without a real TTY in the audit environment, this
becomes a code-level review + targeted live probes against the
actual Bee 2.7.2 node on `localhost:1633`. The findings below
reflect what *would* surprise a fresh operator on first
launch.

Date: 2026-05-07. bee-tui at HEAD post-Phase A/B/C. Bee
2.7.2-rc1, API 8.0.0.

## Verified working

Live API probes via curl confirmed every endpoint bee-tui
polls returns 200:

```
200 /health    200 /status     200 /addresses
200 /chainstate    200 /redistributionstate    200 /reservestate
200 /node      200 /topology   200 /transactions
200 /tags      200 /loggers
```

`examples/v1_5_check.rs` continues to pass against the live
node — `chequebook_address`, `check_pins` (NDJSON), and the
1.6 `set_logger(expr, level)` fix all green.

`bee-tui --version`, `--help`, `--ascii --help`,
`NO_COLOR=1 --version` all succeed. Binary is 2.1 MiB stripped.

## Audit findings

### Severity: green (ship as-is)

- **Cold-start UX is reassuring.** Every screen has explicit
  `loading…` text on the header line until first response,
  plus per-body empty-state messages ("(no batches yet — buy
  one with swarm-cli or `bee stamps buy`)" etc.).
- **503 syncing is rendered as warn, not fail** — the
  `theme::classify_header_error` heuristic catches the most
  common cold-start error and softens it. Operators on a
  fresh Bee that hasn't finished warmup don't see a wall of
  red.
- **Empty-list drills no-op cleanly.** Both S2 and S6
  `maybe_start_drill` early-return when there are no rows;
  pressing Enter on a "(no batches yet)" placeholder doesn't
  fire a request to a malformed URL.
- **Late-result race is handled.** Drill-pane `drain_fetches`
  silently drops results that no longer match the current
  drill target — operator can Esc → move → Enter on a
  different row and the slow original fetch won't paint over
  the new one.
- **Modal dispatch works correctly.** `?` overlay swallows
  the opening *and* closing key press; an Esc dismissing the
  overlay doesn't also collapse a drill underneath.
- **Glyph slot system actually swaps.** The seven Unicode
  glyphs across the codebase all migrated to
  `theme::active().glyphs.X`; the unit test pins the ASCII
  variant at ≤4 chars per glyph for column stability.
- **bee-rs full coverage.** Every endpoint Bee 8.0.0 advertises
  has a bee-rs method. The latent `set_logger_verbosity` bug
  was caught + fixed in 1.6.0.

### Severity: yellow (post-1.0 polish)

- **No live spinner on cold start.** Screens show static
  `loading…` until first response; the redraw loop is
  running (every Tick) but visually there's no indication
  the app is alive vs hung. A 1-char spinner cycling
  `⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏` (or ASCII `|/-\\`) on the loading line would
  fix this. Defer to operator beta — if no one mentions it,
  not worth the wiring.
- **`?` overlay is fixed-size 72×22.** Min'd against area
  dimensions so it shrinks on small terminals, but the keymap
  rows wrap awkwardly on <60 cols. Acceptable degraded mode;
  realistic operator terminals are ≥80 cols.
- **S2 / S6 drill can leak background fetches across `:context`
  switches.** A profile switch tears down the `ApiClient`
  backing the watch hub, but in-flight drill tasks still hold
  the old `Arc<ApiClient>` and will land their results into a
  channel that — by then — has been replaced. The result is
  silently dropped (channel sender returns an error which the
  spawn ignores), so no user-visible bug, just wasted bytes.
  Acceptable for v1.0.
- **No way to copy a peer overlay or batch ID from the drill.**
  Operators sometimes want to paste an address into a block
  explorer. With mouse mode off (default), terminal selection
  works fine. Mention in docs.
- **Footer hints don't mention `?`.** The top tab strip does
  ("`:cmd · Tab · ? help`"), and the overlay is reachable from
  any screen, but the per-screen footer at the bottom doesn't
  include `?`. Potential discoverability gap; relying on the
  top hint is fine.

### Severity: red (would block 1.0)

- **None found.** No crashes, no data corruption, no states the
  audit could put the binary into where it would mislead an
  operator into taking the wrong action.

## What R3 *cannot* tell us

A code-level audit can't catch:

- **Layout drift on real terminals.** Different terminal
  emulators apply different glyph widths to `▒ ▇ ░ ✓ ⚠`
  (kitty / WezTerm / iTerm2 are kind, gnome-terminal is
  mostly OK, Windows Terminal is the wildcard). Needs a real
  beta on diverse setups.
- **Long-session memory growth.** The watch hub uses
  `tokio::sync::watch` (replace-on-update, no buffering) and
  the log-capture is a fixed-size ring buffer. Should stay
  flat, but only a 24h+ run will confirm.
- **Operator confusion that the WHY tooltips don't address.**
  E.g. "what should I do if `Bin saturation: 2 starving`?"
  — the tooltip says "manually `connect` more peers or wait —
  kademlia fills bins…". Whether that's actually actionable
  for a real operator is something only a beta can validate.

These are explicitly the work of the v0.9 beta program, not
the pre-ship audit.

## Recommendation

**Ship 1.0** when:
1. Phase A (✓ done), Phase B (✓ done), Phase C (✓ scaffolded)
   are merged.
2. The README + this R3 report are reviewed by at least one
   second pair of eyes.
3. v1.0.0 tag is pushed; cargo-dist GitHub Actions builds the
   five-platform release; cargo publish runs.
4. awesome-swarm PR + blog post follow within a week.

The remaining yellow items become the v0.9.x patch lane —
shipped as point releases over the first month based on what
the early operators report.
