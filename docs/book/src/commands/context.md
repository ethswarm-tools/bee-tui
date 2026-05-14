# `:context` / `:nodes` — multi-node switching

Switch the cockpit's active node profile without restarting.
The screen layout stays the same; the data behind it
re-points to a different Bee endpoint.

```
:context              # list known profiles (no switch)
:context <name>       # switch to <name>
:ctx <name>           # alias
:nodes                # open the picker overlay (also Ctrl+N)
```

## The picker overlay (added v1.10.0)

`Ctrl+N` (or `:nodes`) opens a centred list of every node in
the session — the `[[nodes]]` entries from `config.toml`, or
the ad-hoc fleet you passed as positional URLs on the command
line (`bee-tui url1 url2 …`, see
[Launching bee-tui](../launching.md)). The cursor lands on the
active node; ↑/↓ (or `j`/`k`) move it, `↵` switches, `Esc` or
`Ctrl+N` close without switching. The active node is marked
`●` and the `default = true` entry is marked `★`. The picker
is just a thin wrapper around the switch flow described
below — same teardown, same rebuild, same status-line
confirmation.

## Listing profiles

With no argument, `:context` lists every profile from your
`config.toml`:

```
usage: :context <name>  (known: prod-1, prod-2, lab)
```

(The "usage" wording is intentional — there's no read-only
mode for the command; `:context` always wants either a target
or to tell you it doesn't have one.)

## Switching

Switching is a clean re-point:

1. Cancel every watcher subscribed to the old hub
2. Build a new `ApiClient` against the named node
3. Spawn a fresh `BeeWatch` hub against the new client
4. Rebuild the screen list against the new watch receivers

The current screen index is preserved — if you were on S6
Peers, you stay on S6 Peers, just looking at a different
node's peers.

The cockpit's status line confirms:

```
switched to context prod-2 (http://10.0.1.6:1633)
```

## What's preserved across a switch

- **Current screen** — your `Tab` cursor doesn't reset.
- **Help overlay state** — if `?` was open, it stays open.
- **Theme + ASCII fallback** — UI prefs are config-level,
  not profile-level.

## What's lost across a switch

A switch is intentionally treated as "fresh slate" — the
same way it would be on app restart. Everything per-screen
that wasn't pulled from the new hub gets reset:

- **Lottery rchash benchmark history** — the in-flight or
  completed bench from the old node is gone. Press `r`
  again to benchmark the new node.
- **Network reachability stability timer** — the "stable
  for 9m" counter restarts at 0 because we have no signal
  yet from the new node.
- **Selection cursors** in S2 / S6 — reset to row 0; the
  underlying batches / peers are different.
- **Drill panes** — any open S2 bucket drill or S6 peer
  drill is closed.
- **Command status line** — replaced with the switch
  confirmation.

The watch streams (S1's 2-second polls, S6's topology, etc.)
re-hydrate within their normal cadence — typically the
cockpit feels live within 5 seconds of the switch.

## Why a switch isn't a restart

Two reasons to keep the cockpit alive across a switch:

1. **Speed.** A full restart re-parses the config, re-installs
   the global tracing capture, re-bootstraps the terminal —
   2–3 seconds of dead time. A `:context` switch is
   sub-second.
2. **Continuity.** Operators frequently want to compare
   nodes side-by-side ("prod-1 has 142 peers; what does
   prod-2 have?"). The screen index preservation makes that
   trivial: switch, look, switch back.

## What does NOT switch

- **The `default = true` profile** in your `config.toml`.
  `:context` is a runtime-only override; the next launch
  starts on the default again. There's no "remember my last
  profile" persistence — by design, the default node is the
  one most likely to need attention, so launches snap there.

## Tokens across a switch

Each profile carries its own `token` (or `@env:VAR`). The
old token is dropped along with the old `ApiClient`; the
new token is loaded from the new profile's config. Tokens
never cross profiles.

## Common scenarios

### "Quick comparison"

```
:context prod-1
[look at S1 Health]
:context prod-2
[same screen, different node]
:context prod-1
```

### "Lab → prod"

```
:context lab          # default on launch
:context prod-1       # promote to production node
```

The lab token never reaches prod (different profile, different
token).

### "Switch failed"

```
context switch failed: no node configured with name "prd-1"
```

Typo. The original profile is still active; the failed switch
is a no-op (no partial teardown happens before the lookup).

### "I have [[nodes]] entries but no `default = true` set"

bee-tui falls back to the **first** entry in the list — it
doesn't refuse to start. Setting `default = true` on the entry
you want is still recommended so the landing node is explicit
rather than list-order-dependent. See
[Configuration](../config.md#schema-reference).

## See also

- [Configuration](../config.md) — defining the `[[nodes]]`
  entries `:context` switches between
- [The `:command` bar](./bar.md)
