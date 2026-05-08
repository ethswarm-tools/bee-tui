# S13 — Durability Watchlist

A running history of `:durability-check` results, plus the live
state of any `:watch-ref` daemons. The operator-facing answer to
the single most-feared question: **is my data still alive?**

## How rows get here

Every invocation of `:durability-check <ref>` adds one row to S13.
The verb walks the chunk graph rooted at `<ref>` and records the
outcome:

```text
:durability-check <ref>
```

Walker behaviour:

- Fetches the root chunk via `GET /chunks/{ref}`.
- If the root parses as a Mantaray manifest, recursively fetches
  every fork's `self_address`. Forks that carry a target
  reference are counted as leaves but their target's *file
  content* is not chunk-walked further (manifest topology only).
- If the root doesn't parse as a manifest, the single-chunk fetch
  *is* the durability answer.
- Hard cap: **10 000 chunks per walk**. Operators with very large
  manifests get a partial answer marked `truncated` rather than
  a stuck cockpit.
- BMT verification is on by default — every fetched chunk's
  content is keccak-hashed and compared against the requested
  reference. Mismatches land in the separate `chunks_corrupt`
  bucket. Opt-out via `[durability].bmt_verify = false` in
  config.

The rolling history is bounded to the **most recent 50 rows**;
older rows are evicted from the back as new checks land.

## Layout

```
┌  4 checks · 3 healthy · 1 unhealthy ─────────────────────────────────────────┐
│                                                                              │
│ ▸ OK         manifest  ee7f3a20  12 total · 0 lost · 0 errors · BMT · 412ms  4s ago
│   UNHEALTHY  manifest  9c4d9a80  18 total · 1 lost · 0 errors · 1 corrupt · BMT · scan: NOT seen · 1018ms  31s ago
│   OK         chunk     a02ee188  1 total · 0 lost · 0 errors · BMT · 87ms   2m ago
│   OK         manifest  f8aa0f76  120 total · 0 lost · 0 errors · BMT (truncated) · 8841ms  17m ago
│                                                                              │
│   selected: ee7f3a201810c5e9…3e4d1abf                                        │
│  Tab switch screen   ↑↓/jk select   ? help   q quit   :durability-check <ref> to record
└──────────────────────────────────────────────────────────────────────────────┘
```

Each row reports:

| Column | Meaning |
|---|---|
| `OK` / `UNHEALTHY` | Green / red status pill — `is_healthy()` is `true` iff `lost == 0 && errors == 0 && corrupt == 0` |
| `manifest` / `chunk` | Whether the root parsed as a Mantaray manifest |
| short ref | First 8 hex chars of the reference; full hex is on the `selected:` line |
| detail | `<total> total · <lost> lost · <errors> errors · <corrupt> corrupt · BMT · scan: seen/NOT seen · <duration>ms (truncated)` |
| age | Wall-clock time since the check started |

`BMT` appears in detail when the walk verified each chunk's
content against its address; `truncated` appears when the walk
stopped at the 10 000-chunk cap; the swarmscan segment appears
only when `[durability].swarmscan_check = true`.

## The four outcome buckets

S13 separates four counts with different operator implications:

| Bucket | Meaning | Likely cause |
|---|---|---|
| `lost` | `GET /chunks/{ref}` returned 404 | Network truly dropped your data — check stamp TTL, peer reachability, batch utilisation |
| `errors` | Anything else (timeout, 500, decode error) | Flaky local node or transient network — retry usually fixes |
| `corrupt` | Content fetched but BMT hash didn't match the requested reference | Bit-rot, swap-corrupted on-disk chunk, or hostile peer returning a different chunk |
| (rest) | Successfully retrieved + verified | Healthy |

## Optional swarmscan cross-check

When `[durability].swarmscan_check = true` is set in the
configuration, the walker — **after** the local walk completes —
also probes a swarmscan-style indexer for the same reference:

```toml
[durability]
swarmscan_check = true
swarmscan_url   = "https://api.swarmscan.io/v1/chunks/{ref}"  # default
```

The probe replaces `{ref}` with the hex-encoded reference and
expects a 200 (seen) or 404 (not seen). Anything else (timeout,
non-200/404) renders as no answer (`scan: ` segment is hidden).

This gives an **independent network-side answer** —  "the
indexer says the network sees this ref" — separate from "my
local node was able to retrieve it." Useful when triaging:

- Healthy + scan: seen → all good.
- Healthy + scan: NOT seen → your local node has it cached; the
  network may have dropped the rest. Re-upload before your
  cache expires.
- Unhealthy + scan: seen → your local node is the problem; the
  network has the ref. Restart, re-sync, or check connectivity.
- Unhealthy + scan: NOT seen → genuine data loss. Re-upload from
  the source if you still have it.

## Daemon mode (`:watch-ref`)

For a continuous answer, run `:watch-ref` as a daemon:

```text
:watch-ref      <ref> [interval-seconds]   # default 60s, clamped 10..=86400
:watch-ref-stop [ref]                      # cancel one (or all if no arg)
```

`:watch-ref` re-runs `:durability-check` on a tokio interval and
records each result on S13 — same row format as a manual
`:durability-check`. Re-issuing for an already-watched ref
cancels the prior daemon (clean restart). The cockpit's root
cancellation token also fires on quit, so daemons clean up
without operator action.

See [`:watch-ref` daemon mode](./../commands/watch-ref.md) for
the full verb reference.

## Keymap

| Key | Action |
|---|---|
| `↑` / `k` | Move cursor up |
| `↓` / `j` | Move cursor down |
| `Tab` | Cycle to the next screen |
| `:` | Open the command bar |

## What S13 isn't

- **Not persisted across cockpit restarts.** The history is an
  in-memory ring buffer; quitting bee-tui drops it. If you want
  durable history, redirect the verb's stdout from `--once
  durability-check` into a JSONL file from cron (the JSON shape
  is part of the v1.3.0 stable surface).
- **Not a fixer.** S13 surfaces the diagnosis; remediation
  (`:reupload`, manifest re-binding, stamp top-up) lives in the
  deferred [write tier](../commands/bar.md#whats-not-on-the-bar).
- **Not a content checker.** A manifest's leaves point at file
  content that is itself chunked; the walker only verifies the
  manifest topology + each chunk it visits, not the file content
  reachable through leaves. A leaf reporting "OK" means the
  Mantaray fork loaded cleanly; the file's individual chunks
  are a separate `:durability-check` away.
- **Not a CI gate.** For automation, use
  [`--once durability-check`](./../commands/bar.md) — it exits
  `1` on unhealthy, `2` on usage error, and emits the same
  result shape as a JSON object via `--json`.
