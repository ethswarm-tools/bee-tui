# The bottom command-log pane

> **Naming note.** This page is named `s10-log.md` for legacy
> reasons. In v0.1 the command log *was* the tenth screen
> (`S10`); since v0.9 it's been a persistent **pane at the
> bottom of every screen**, not a screen of its own. The
> current numbered screens are S1 Health through S14 Pubsub —
> all 14 of them tab-cycled through the screen strip. The log
> pane is always visible underneath. The file is kept at its
> old path so existing bookmarks resolve.

A lazygit-style append-only tail of every HTTP request the
cockpit makes to Bee. The trust anchor and live tutorial:
operators see the *actual* request behind every gauge they're
watching, with method, path, status, and elapsed time.

## Reading the pane in detail (v1.13+)

Two viewing-comfort affordances were added in v1.13.0:

- **`Shift+L`** toggles a **fullscreen** view of the log pane.
  The active screen above collapses to zero lines and the pane
  expands to fill the middle of the cockpit — same data, same
  seven tabs (`[` / `]` to cycle), same filter (see below). Press
  `Shift+L` again to return to the split layout. Top bar and
  command bar stay visible either way, so you never lose sight
  of which node you're on.
- **`/`** opens an in-pane **filter** prompt — case-insensitive
  substring. Type a needle, press `Enter`, and the pane hides
  every line that doesn't match. The title strip shows
  `/<query> · N matches` so you can confirm at a glance that
  your filter is doing what you expect. `Esc` cancels the
  prompt; pressing `Esc` again (no prompt open) clears an
  active filter. The filter applies to whichever tab you're
  viewing — switching tabs (`[` / `]`) keeps it applied, so
  you can grep "where did `503 syncing` show up" across
  Errors, Bee HTTP, and the cockpit log without retyping.

The filter is a **substring** match, not a regex. It's
case-insensitive and respects the live tail — newly arriving
lines that match appear at the bottom; non-matching lines are
silently skipped over. The match count updates as new lines
arrive even while you have the pane scrolled back.



## Why this screen exists

Three reasons, in priority order:

1. **Trust** — when the cockpit says "Bin saturation: 7
   starving", an operator with a healthy paranoia wants to
   verify it's not a render bug. S10 shows the literal
   `GET /topology` that produced the answer.
2. **Live tutorial** — every cockpit gauge is fed by some Bee
   endpoint. New operators can use S10 as a "Bee API by
   example" — see what's polled, what's NDJSON-streamed, what
   fires only on user action.
3. **Debug aid** — when something is failing (401, 503,
   connection refused), the failure shows up here in
   real-time. Way faster than attaching a debugger to Bee.

## What's logged

Every HTTP call made via the bee-rs `ApiClient` is captured
by the global `LogCapture` (installed at startup) and
rendered in S10. That includes:

- Periodic polls (`/health`, `/status`, `/wallet`, `/chainstate`,
  `/redistributionstate`, `/stamps`, `/topology`, `/tags`, …)
- On-demand drill fetches (`/stamps/<id>/buckets`, the
  per-peer drill fan-out, rchash, etc.)
- Slash-command requests (`:pins-check`, `:loggers`,
  `:set-logger`)

Bearer tokens are *never* logged. The capture sees `method`,
`url`, `status`, `elapsed_ms`, `ts` — never headers.

## The display

```
 bee::http
  08:12:01.123  GET    /health                        200    34ms
  08:12:01.456  GET    /chainstate                    200    18ms
  08:12:03.001  GET    /redistributionstate           200    21ms
  08:12:03.456  GET    /wallet                        200    15ms
  08:12:05.001  GET    /status                        200    12ms
  08:12:05.222  GET    /tags                          200    45ms
  08:12:05.500  GET    /stamps/abc123…/buckets        200   2840ms
  08:12:05.700  GET    /pingpong/aaa…aaa              200     6ms
  08:12:05.701  GET    /peers/aaa…aaa/balance         200    11ms
  08:12:05.702  GET    /settlements/aaa…aaa           200    14ms
  08:12:05.703  GET    /chequebook/cheque/aaa…aaa     200    19ms
```

Columns:

| Column | Meaning |
|---|---|
| Timestamp | Local time when the request started |
| Method | `GET` (blue), `POST` (green), `PUT` (yellow), `DELETE` (red), `PATCH` (magenta), `HEAD` (cyan) |
| Path | URL path, scheme + host stripped |
| Status | HTTP status code, colour-coded |
| Elapsed | Round-trip time in ms |

### Status colour coding

- `2xx` — green (success)
- `3xx` — info-blue (redirect; rare in Bee)
- `4xx` — warn-yellow (client error: 401 auth, 404 missing, 503 syncing)
- `5xx` — fail-red (server error)
- `—` — dim (request didn't complete; connection refused, timeout)

### Path stripping

Scheme + host are dropped so the line stays readable on
80-col terminals. `http://localhost:1633/health` renders as
`/health`. Query strings are kept (visible on `/chunks/stream`).

## How big is the buffer?

200 entries, ring buffer. Older entries fall off as new ones
arrive. At a typical poll cadence (~10 calls/sec across
streams + polls), you have ~20 s of recent history. That's
enough to debug "what just happened" but not enough for
long-term forensics.

If you need more history, use `:diagnose` — it dumps the
*entire* current buffer plus snapshot state to
`$TMPDIR/bee-tui-diagnostic-<ts>.txt`.

## Reading patterns

### "I just tabbed to a screen and saw 4 calls"

That's the screen activating its on-tab fetches. S2 fires
`/stamps`. S3 fires the chequebook + settlements set. S6
fires `/topology` (already in the shared stream so often
no new call). S8 fires `/transactions`.

### "Same path repeating every 2 seconds"

That's a poller. The cadence per endpoint is documented in
each screen's "Snapshot cadence" section.

### "Path with `?` query string"

WebSocket upgrades + on-demand commands. `:pins-check`
fires `/pins/check` with optional reference query, etc.

### "503 status repeating"

Bee is syncing. This is the cold-start "bee is syncing
chunks, gauges will hydrate within ~10 minutes" pattern. See
[First run](../first-run.md).

### "401 status"

Auth token mismatch. Either:

- The token in your config doesn't match Bee's `--api-token`
- `@env:VAR` resolved to an empty string (unset env var)
- Bee was restarted with a new token

Check S1 Health gate 1 (API reachable) and your config.

### "PUT /loggers/..." with a long base64 path

That's `:set-logger` (or the legacy v1 endpoint). The base64
chunk is the URL-safe-encoded logger expression.

## Common scenarios

### "Cockpit feels slow"

Watch the `Elapsed` column. If most calls are <100ms, the
slowness is in render, not Bee. If many calls are 500ms+,
Bee is the bottleneck. Drop to S8 RPC / API health for the
p50 / p99 over the last 100 calls.

### "I want to trust the chequebook number"

Watch S10 while looking at S3. You'll see
`GET /chequebook/balance` returning a 200 every 30 seconds.
Compare the cockpit's display with `curl
http://localhost:1633/chequebook/balance` from a separate
shell — they'll match.

### "I'm writing my own Bee client and want to know what calls to make"

Watch S10 while flipping through every screen. Every endpoint
the cockpit uses is in the bee-rs ApiClient (mirror in
bee-py / bee-go); seeing them in real time is faster than
reading the OpenAPI spec.

## What this screen *doesn't* show

- **WebSocket frames** — the cockpit subscribes to
  `/chunks/stream` (potentially) but the tail only shows the
  upgrade request, not individual frames.
- **Internal state changes** — only HTTP calls. The
  cockpit's own snapshot diffs / cache invalidations don't
  appear here.
- **Bee server logs (on the `bee::http` tab)** — that tab is
  bee-tui's *own* outbound HTTP calls. Bee's own log output
  shows up on the **Errors / Warn / Info / Debug / Bee HTTP**
  tabs instead — when bee-tui has a log source. As of v1.15
  that's usually automatic for a local node: bee-tui spawned
  Bee (`[bee]` / `--bee-bin`), or auto-discovery found the
  local Bee process and tailed its log file / systemd journal /
  docker logs, or you set `[[nodes]].log_file` / `log_command`
  (or `--bee-log` / `--bee-log-cmd`) explicitly. When the tabs
  *are* empty, the placeholder says why — including the
  actionable case where a local Bee logs to a bare terminal
  that can't be captured.

## Cadence

S10 doesn't poll. It subscribes to the live LogCapture
process-wide and renders whatever's in the buffer at draw
time (60 fps — but only repaints when entries change).

## Keys

S10 has no screen-specific keys. The global keymap (`Tab`,
`?`, `:`, `q`) covers everything.

If you want to *export* a slice of the log, use `:diagnose`
which captures the full buffer to a file alongside the
snapshot state.
