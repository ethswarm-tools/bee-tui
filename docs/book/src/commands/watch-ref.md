# `:watch-ref` / `:watch-ref-stop`

Daemon mode for [`:durability-check`](./pins-check.md). Runs the
chunk-graph walk on a reference periodically and feeds each result
into the [S12 Watchlist](../screens/s13-watchlist.md) — the same screen
single-shot `:durability-check` already populates. Useful for
"watch this ref overnight" workflows where you want to know the
moment a chunk goes missing.

```text
:watch-ref       <ref> [interval-secs]
:watch-ref-stop  [ref]
```

## Starting a daemon

`<ref>` is a 64-character hex Swarm reference (32 bytes, with or
without the `0x` prefix).

`[interval-secs]` is optional, defaults to **60 s**, and is clamped
to the inclusive range `10..=86_400` (10 s to one day). Below 10 s
the per-chunk fetch storm crowds out other cockpit polling; above
one day the cockpit's tick cadence makes the daemon nearly
indistinguishable from a manual re-run.

```text
:watch-ref e7f3a201cd1f0e9b… 300
→ watch-ref e7f3a201 started — re-checking every 300s; results in S12 Watchlist
```

Each iteration runs the **full BMT-verified** durability walk
shipped in v1.5; new `chunks_corrupt` counts surface in the S13
row alongside `chunks_lost` / `chunks_errors`.

Re-issuing `:watch-ref` for a ref already being watched cancels
the prior daemon and starts a fresh one — convenient for changing
the interval without an explicit stop:

```text
:watch-ref e7f3a201cd1f0e9b… 60
→ watch-ref e7f3a201 started — re-checking every 60s; results in S12 Watchlist
```

## Stopping a daemon

```text
:watch-ref-stop                   # cancels every active daemon
:watch-ref-stop e7f3a201cd1f0…    # cancels just the one watching this ref
```

A daemon's tokio task observes the cancel on its next iteration
boundary — up to `interval-secs` later if a check is in flight or
the sleep is mid-window. The cockpit's hashmap entry (and the
"X active daemon(s)" count in `:watch-ref-stop` with no arg) is
updated immediately.

The cockpit's root cancellation token also fires on quit, so
operators don't need to remember to issue `:watch-ref-stop`
before exiting.

## Output

The verb itself is synchronous (just spawns the loop). Each
periodic check's result lands in the S12 Watchlist row history
the same way a manual `:durability-check` does — newest first,
ring-buffered to the screen's row cap.

## When to use it

- **Overnight monitoring** of a known ref. Pair with `[alerts]`
  to get a webhook when the durability gate flips on the last
  iteration's outcome (v1.4 alerting + v1.6 watch-ref are
  designed to compose).
- **Verifying a freshly published ref propagates.** Set a 30 s
  interval after `:upload-collection` and watch the `lost`
  count converge to zero as the network catches up.
- **Catching transient peer churn.** A single `:durability-check`
  may report `errors=1` from a flaky peer; a daemon at 5 min
  intervals shows whether the issue is persistent.

## What it doesn't do

- **No state persistence.** Daemons live in App memory only;
  cockpit restart drops them. Re-issue from a startup script if
  you want them restored.
- **No swarmscan cross-check yet.** The original v1.6 plan
  mentioned a swarmscan probe ("does the network see this ref
  independent of my local node"); deferred to v1.7. Each
  iteration today asks only the local Bee node.
- **No per-ref interval override after start.** Re-issue
  `:watch-ref <ref> <new-interval>` to swap; the prior daemon is
  cancelled before the new one starts.
